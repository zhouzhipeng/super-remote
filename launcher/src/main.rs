#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("Super Remote is only available on Windows");
}

#[cfg(windows)]
mod windows_launcher {
    use std::{
        collections::BTreeMap,
        env,
        ffi::OsStr,
        fs::{self, OpenOptions},
        io::{Read, Write},
        net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
        os::windows::process::CommandExt,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use anyhow::{Context, bail};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use qrcode::{QrCode, render::svg};
    use rand::RngCore;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use windows::{
        Win32::{
            Foundation::{CloseHandle, ERROR_BUFFER_OVERFLOW, HWND, NO_ERROR},
            Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW},
            NetworkManagement::{
                IpHelper::{
                    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
                    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD,
                    IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH,
                },
                Ndis::IfOperStatusUp,
            },
            Networking::WinSock::{AF_INET, SOCKADDR_IN},
            System::Threading::{
                OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
                },
                Shell::{IsUserAnAdmin, ShellExecuteW},
                WindowsAndMessaging::{
                    GetSystemMetrics, MB_ICONERROR, MB_OK, MessageBoxW, SM_CXVIRTUALSCREEN,
                    SM_CYVIRTUALSCREEN, SW_SHOWNORMAL,
                },
            },
        },
        core::{HSTRING, PWSTR, w},
    };

    const DEFAULT_PORT: u16 = 8080;
    const TURN_TCP_PORT: u16 = 3478;
    const TURN_RELAY_MIN_PORT: u16 = 49160;
    const TURN_RELAY_MAX_PORT: u16 = 49200;
    const TURN_REALM: &str = "super-remote";
    const DEVICE_ID: &str = "local-windows-pc";
    const PERMANENT_EXPIRY: usize = 253_402_300_799;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    #[derive(Clone, Copy)]
    struct VideoPipeline {
        encoder: &'static str,
        capture_mode: &'static str,
        label: &'static str,
        capture_label: &'static str,
        fps: u16,
        bitrate: u32,
        max_width: Option<u32>,
    }

    struct ManagedChild(Child);

    impl std::ops::Deref for ManagedChild {
        type Target = Child;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl std::ops::DerefMut for ManagedChild {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl Drop for ManagedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Credentials {
        jwt_secret: String,
        device_token: String,
        #[serde(default)]
        turn_secret: String,
        #[serde(default = "default_username")]
        username: String,
        password: String,
        #[serde(default = "default_port")]
        port: u16,
    }

    #[derive(Serialize)]
    struct AccessClaims<'a> {
        sub: &'a str,
        role: &'static str,
        exp: usize,
        iat: usize,
    }

    #[derive(Serialize)]
    struct LauncherStatus<'a> {
        url: &'a str,
        direct_url: &'a str,
        qr: String,
        username: &'a str,
        password: &'a str,
        port: u16,
        signaling_pid: u32,
        host_pid: u32,
        turn_pid: u32,
        turn_url: String,
        turn_urls: Vec<String>,
        turn_relay_ports: String,
        launcher_pid: u32,
        desktop: String,
        primary_display: String,
        stream: String,
        encoder: &'static str,
        capture_mode: &'static str,
        elevated: bool,
        launcher_executable: String,
        data_dir: String,
    }

    pub fn run() -> anyhow::Result<()> {
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            relaunch_elevated()?;
            return Ok(());
        }

        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .context("无法启用 DPI 感知")?;
        let root = application_root()?;
        let data_dir = application_data_dir();
        fs::create_dir_all(&data_dir).context("无法创建程序数据目录")?;
        env::set_current_dir(&root).context("无法进入安装目录")?;
        stop_existing_stack(&root, &data_dir)?;

        let uninstalling = env::args().any(|argument| argument == "--uninstall");
        if env::args().any(|argument| argument == "--stop" || argument == "--uninstall") {
            if uninstalling {
                stop_control_panel(&root, &data_dir)?;
            }
            remove_firewall_rules(&root);
            return Ok(());
        }

        require_runtime_files(&root)?;
        let (primary_width, primary_height, desktop_width, desktop_height) = display_geometry()?;
        let video_pipeline = select_video_pipeline(&root)?;
        let (stream_width, stream_height) =
            stream_dimensions(primary_width, primary_height, video_pipeline.max_width);
        let ip = lan_ip()?;
        let credentials = load_credentials(&data_dir)?;
        validate_credentials(&credentials)?;
        let base_url = format!("http://{}:{}", ip, credentials.port);
        let access_token = permanent_access_token(&credentials.jwt_secret, &credentials.username)?;
        let direct_url = format!(
            "{base_url}/?v={}#token={access_token}&device={DEVICE_ID}",
            unix_seconds()
        );

        write_host_config(
            &data_dir,
            &base_url,
            &credentials.device_token,
            &root.join("ffmpeg.exe"),
            stream_width,
            stream_height,
            primary_width,
            primary_height,
            video_pipeline,
        )?;
        configure_firewall(&root, credentials.port)?;

        let common_environment = BTreeMap::from([
            ("REMOTE_BIND", format!("0.0.0.0:{}", credentials.port)),
            ("REMOTE_JWT_SECRET", credentials.jwt_secret.clone()),
            ("REMOTE_ADMIN_USER", credentials.username.clone()),
            ("REMOTE_ADMIN_PASSWORD", credentials.password.clone()),
            ("REMOTE_DEVICE_TOKEN", credentials.device_token.clone()),
            (
                "REMOTE_TURN_URLS",
                format!(
                    "turn:{ip}:{}?transport=udp,turn:{ip}:{TURN_TCP_PORT}?transport=tcp",
                    credentials.port
                ),
            ),
            ("REMOTE_TURN_SECRET", credentials.turn_secret.clone()),
            ("RUST_LOG", "remote_signaling=info,remote_host=info".into()),
            (
                "SUPER_REMOTE_DATA_DIR",
                data_dir.to_string_lossy().into_owned(),
            ),
        ]);

        let mut turn = spawn_logged(
            &root.join("remote-turn.exe"),
            &[
                "--public-ip".into(),
                ip.to_string(),
                "--realm".into(),
                TURN_REALM.into(),
                "--tcp-port".into(),
                TURN_TCP_PORT.to_string(),
                "--udp-port".into(),
                credentials.port.to_string(),
                "--min-port".into(),
                TURN_RELAY_MIN_PORT.to_string(),
                "--max-port".into(),
                TURN_RELAY_MAX_PORT.to_string(),
            ],
            &root,
            &data_dir.join("turn.log"),
            &common_environment,
        )?;
        wait_for_tcp_listener(
            ip,
            TURN_TCP_PORT,
            &mut turn,
            "TURN",
            Duration::from_secs(10),
        )?;

        let mut signaling = spawn_logged(
            &root.join("remote-signaling.exe"),
            &[],
            &root,
            &data_dir.join("signaling.log"),
            &common_environment,
        )?;
        wait_for_health(
            ip,
            credentials.port,
            &mut signaling,
            Duration::from_secs(20),
        )?;

        let config_path = data_dir.join("remote-host.toml");
        let mut host = spawn_logged(
            &root.join("remote-host.exe"),
            &[config_path.to_string_lossy().into_owned()],
            &root,
            &data_dir.join("host.log"),
            &common_environment,
        )?;
        wait_for_device(
            ip,
            credentials.port,
            &access_token,
            &mut host,
            Duration::from_secs(20),
        )?;

        let qr_path = data_dir.join("remote-desktop-qr.svg");
        write_qr_code(&qr_path, &direct_url)?;
        let launcher_executable = env::current_exe()?.to_string_lossy().into_owned();
        let status = LauncherStatus {
            url: &base_url,
            direct_url: &direct_url,
            qr: qr_path.to_string_lossy().into_owned(),
            username: &credentials.username,
            password: &credentials.password,
            port: credentials.port,
            signaling_pid: signaling.id(),
            host_pid: host.id(),
            turn_pid: turn.id(),
            turn_url: format!("turn:{ip}:{}?transport=udp", credentials.port),
            turn_urls: vec![
                format!("turn:{ip}:{}?transport=udp", credentials.port),
                format!("turn:{ip}:{TURN_TCP_PORT}?transport=tcp"),
            ],
            turn_relay_ports: format!("{TURN_RELAY_MIN_PORT}-{TURN_RELAY_MAX_PORT}/udp"),
            launcher_pid: std::process::id(),
            desktop: format!("{desktop_width}x{desktop_height}"),
            primary_display: format!("{primary_width}x{primary_height}"),
            stream: format!("{stream_width}x{stream_height}"),
            encoder: video_pipeline.label,
            capture_mode: video_pipeline.capture_label,
            elevated: true,
            launcher_executable,
            data_dir: data_dir.to_string_lossy().into_owned(),
        };
        write_json(&data_dir.join("status.json"), &status)?;
        let shutdown_marker = data_dir.join(format!("shutdown-{}.requested", std::process::id()));

        let mut panel = Command::new(root.join("remote-control-panel.exe"));
        panel
            .arg(&root)
            .env("SUPER_REMOTE_DATA_DIR", &data_dir)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
        let _ = panel.spawn();

        loop {
            if shutdown_marker.is_file() {
                let _ = fs::remove_file(&shutdown_marker);
                return Ok(());
            }
            let exits = [
                ("TURN", turn.try_wait()?),
                ("Signaling", signaling.try_wait()?),
                ("Host", host.try_wait()?),
            ];
            if let Some((name, status)) = exits
                .into_iter()
                .find_map(|(name, status)| status.map(|s| (name, s)))
            {
                bail!("{name} 服务意外退出（{status}），请检查 {name} 日志");
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    fn default_username() -> String {
        "admin".into()
    }

    const fn default_port() -> u16 {
        DEFAULT_PORT
    }

    fn application_root() -> anyhow::Result<PathBuf> {
        let executable = env::current_exe()?.canonicalize()?;
        let binary_dir = executable.parent().context("启动器没有父目录")?;
        if binary_dir.join("remote-host.exe").is_file() {
            return Ok(binary_dir.to_path_buf());
        }
        binary_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .context("无法确定项目根目录")
    }

    fn application_data_dir() -> PathBuf {
        if let Some(path) = env::var_os("SUPER_REMOTE_DATA_DIR") {
            return PathBuf::from(path);
        }
        env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("Super Remote")
    }

    fn require_runtime_files(root: &Path) -> anyhow::Result<()> {
        for name in [
            "remote-turn.exe",
            "remote-signaling.exe",
            "remote-host.exe",
            "remote-control-panel.exe",
            "ffmpeg.exe",
        ] {
            if !root.join(name).is_file() {
                bail!("安装不完整，缺少 {name}");
            }
        }
        Ok(())
    }

    fn relaunch_elevated() -> anyhow::Result<()> {
        let executable = env::current_exe()?;
        let arguments = env::args_os()
            .skip(1)
            .map(|value| quote_argument(&value))
            .collect::<Vec<_>>()
            .join(" ");
        let executable = HSTRING::from(executable.as_os_str());
        let arguments = HSTRING::from(arguments);
        let directory = HSTRING::from(application_root()?.as_os_str());
        let result = unsafe {
            ShellExecuteW(
                Some(HWND::default()),
                w!("runas"),
                &executable,
                &arguments,
                &directory,
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            bail!(
                "管理员权限请求被取消或启动失败（错误 {}）",
                result.0 as isize
            );
        }
        Ok(())
    }

    fn quote_argument(value: &OsStr) -> String {
        let value = value.to_string_lossy();
        if !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c == '"') {
            return value.into_owned();
        }
        let mut result = String::from("\"");
        let mut slashes = 0;
        for character in value.chars() {
            match character {
                '\\' => slashes += 1,
                '"' => {
                    result.push_str(&"\\".repeat(slashes * 2 + 1));
                    result.push('"');
                    slashes = 0;
                }
                _ => {
                    result.push_str(&"\\".repeat(slashes));
                    slashes = 0;
                    result.push(character);
                }
            }
        }
        result.push_str(&"\\".repeat(slashes * 2));
        result.push('"');
        result
    }

    fn display_geometry() -> anyhow::Result<(u32, u32, i32, i32)> {
        let mut mode = DEVMODEW::default();
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        if !unsafe { EnumDisplaySettingsW(None, ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool() {
            bail!("无法读取主显示器物理分辨率");
        }
        let primary_width = mode.dmPelsWidth;
        let primary_height = mode.dmPelsHeight;
        let desktop_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let desktop_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if primary_width == 0 || primary_height == 0 || desktop_width <= 0 || desktop_height <= 0 {
            bail!("显示器尺寸无效");
        }
        Ok((primary_width, primary_height, desktop_width, desktop_height))
    }

    fn lan_ip() -> anyhow::Result<Ipv4Addr> {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let mut required_bytes = 15_000_u32;
        for _ in 0..3 {
            // A u64 backing allocation provides sufficient alignment for the linked
            // IP_ADAPTER_ADDRESSES structures returned into this variable-sized buffer.
            let mut buffer = vec![0_u64; (required_bytes as usize).div_ceil(size_of::<u64>())];
            let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            let result = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_INET.0),
                    flags,
                    None,
                    Some(first),
                    &mut required_bytes,
                )
            };
            if result == ERROR_BUFFER_OVERFLOW.0 {
                continue;
            }
            if result != NO_ERROR.0 {
                bail!("Windows 网卡枚举失败（错误 {result}）");
            }

            let mut best: Option<(u32, Ipv4Addr)> = None;
            let mut adapter = first;
            while !adapter.is_null() {
                let item = unsafe { &*adapter };
                if item.OperStatus == IfOperStatusUp
                    && matches!(item.IfType, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211)
                    && !item.FirstGatewayAddress.is_null()
                {
                    let mut unicast = item.FirstUnicastAddress;
                    while !unicast.is_null() {
                        let socket = unsafe { (*unicast).Address.lpSockaddr };
                        if !socket.is_null() && unsafe { (*socket).sa_family } == AF_INET {
                            let ipv4 = unsafe { &*socket.cast::<SOCKADDR_IN>() };
                            let octets = unsafe { ipv4.sin_addr.S_un.S_un_b };
                            let address =
                                Ipv4Addr::new(octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4);
                            if address.is_private()
                                && best.is_none_or(|(metric, _)| item.Ipv4Metric < metric)
                            {
                                best = Some((item.Ipv4Metric, address));
                            }
                        }
                        unicast = unsafe { (*unicast).Next };
                    }
                }
                adapter = item.Next;
            }
            return best
                .map(|(_, address)| address)
                .context("找不到已连接、带默认网关的以太网或 Wi-Fi 局域网 IPv4 地址");
        }
        bail!("Windows 网卡列表在读取时持续变化，请稍后重试")
    }

    fn random_secret(bytes: usize) -> String {
        let mut buffer = vec![0_u8; bytes];
        rand::rng().fill_bytes(&mut buffer);
        URL_SAFE_NO_PAD.encode(buffer)
    }

    fn load_credentials(data_dir: &Path) -> anyhow::Result<Credentials> {
        let path = data_dir.join("secrets.json");
        let mut credentials = if path.is_file() {
            serde_json::from_slice::<Credentials>(&fs::read(&path)?).context("凭据文件格式无效")?
        } else {
            Credentials {
                jwt_secret: random_secret(48),
                device_token: random_secret(36),
                turn_secret: random_secret(48),
                username: default_username(),
                password: random_secret(12),
                port: DEFAULT_PORT,
            }
        };
        if credentials.turn_secret.is_empty() {
            credentials.turn_secret = random_secret(48);
        }
        write_json(&path, &credentials)?;
        Ok(credentials)
    }

    fn validate_credentials(credentials: &Credentials) -> anyhow::Result<()> {
        if credentials.jwt_secret.len() < 32
            || credentials.device_token.len() < 24
            || credentials.turn_secret.len() < 32
        {
            bail!("本机凭据长度无效，请删除 secrets.json 后重新启动");
        }
        if credentials.username.trim().is_empty()
            || credentials.password.len() < 12
            || credentials.port == 0
        {
            bail!("登录账号、密码或端口配置无效");
        }
        Ok(())
    }

    fn permanent_access_token(secret: &str, subject: &str) -> anyhow::Result<String> {
        let now = unix_seconds() as usize;
        Ok(encode(
            &Header::new(Algorithm::HS256),
            &AccessClaims {
                sub: subject,
                role: "user",
                exp: PERMANENT_EXPIRY,
                iat: now,
            },
            &EncodingKey::from_secret(secret.as_bytes()),
        )?)
    }

    fn write_host_config(
        data_dir: &Path,
        base_url: &str,
        device_token: &str,
        ffmpeg_path: &Path,
        stream_width: u32,
        stream_height: u32,
        capture_width: u32,
        capture_height: u32,
        pipeline: VideoPipeline,
    ) -> anyhow::Result<()> {
        let status_path = data_dir
            .join("host-state.json")
            .to_string_lossy()
            .into_owned();
        let quoted =
            |value: &str| serde_json::to_string(value).expect("JSON strings are TOML-compatible");
        let config = format!(
            "server_url = {}\ndevice_id = {}\ndevice_name = {}\ndevice_token = {}\nwidth = {stream_width}\nheight = {stream_height}\nfps = {}\nbitrate = {}\nmonitor_index = 0\nffmpeg_path = {}\nffmpeg_encoder = {}\nffmpeg_capture_mode = {}\nffmpeg_capture_x = 0\nffmpeg_capture_y = 0\nffmpeg_capture_width = {capture_width}\nffmpeg_capture_height = {capture_height}\ncontrol_status_path = {}\n\n[[ice_servers]]\nurls = [\"stun:stun.l.google.com:19302\"]\n",
            quoted(base_url),
            quoted(DEVICE_ID),
            quoted("这台 Windows 电脑"),
            quoted(device_token),
            pipeline.fps,
            pipeline.bitrate,
            quoted(&ffmpeg_path.to_string_lossy()),
            quoted(pipeline.encoder),
            quoted(pipeline.capture_mode),
            quoted(&status_path)
        );
        fs::write(data_dir.join("remote-host.toml"), config)?;
        Ok(())
    }

    fn select_video_pipeline(root: &Path) -> anyhow::Result<VideoPipeline> {
        let ffmpeg = root.join("ffmpeg.exe");
        if probe_ffmpeg_encoder(&ffmpeg, "h264_nvenc") {
            return Ok(VideoPipeline {
                encoder: "h264_nvenc",
                capture_mode: "ddagrab",
                label: "NVIDIA NVENC H.264",
                capture_label: "Desktop Duplication",
                fps: 60,
                bitrate: 20_000_000,
                max_width: None,
            });
        }
        if probe_ffmpeg_encoder(&ffmpeg, "h264_amf") {
            return Ok(VideoPipeline {
                encoder: "h264_amf",
                capture_mode: "gdigrab",
                label: "AMD AMF H.264",
                capture_label: "Windows GDI Capture",
                fps: 30,
                bitrate: 12_000_000,
                max_width: Some(1920),
            });
        }
        if probe_ffmpeg_encoder(&ffmpeg, "libx264") {
            return Ok(VideoPipeline {
                encoder: "libx264",
                capture_mode: "gdigrab",
                label: "FFmpeg H.264 (software)",
                capture_label: "Windows GDI Capture",
                fps: 30,
                bitrate: 8_000_000,
                max_width: Some(1600),
            });
        }
        bail!("内置 FFmpeg 无法初始化任何 H.264 编码器")
    }

    fn probe_ffmpeg_encoder(ffmpeg: &Path, encoder: &str) -> bool {
        Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=1920x1080:r=1",
                "-frames:v",
                "1",
                "-an",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                encoder,
                "-f",
                "null",
                "NUL",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|status| status.success())
    }

    fn stream_dimensions(width: u32, height: u32, max_width: Option<u32>) -> (u32, u32) {
        let Some(max_width) = max_width else {
            return (width, height);
        };
        if width <= max_width {
            return (width, height);
        }
        let scaled_height =
            ((u64::from(height) * u64::from(max_width) / u64::from(width)) as u32).max(2) & !1;
        (max_width & !1, scaled_height)
    }

    fn spawn_logged(
        executable: &Path,
        arguments: &[String],
        root: &Path,
        log_path: &Path,
        environment: &BTreeMap<&str, String>,
    ) -> anyhow::Result<ManagedChild> {
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        let stderr = stdout.try_clone()?;
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .current_dir(root)
            .envs(environment)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        command
            .spawn()
            .map(ManagedChild)
            .with_context(|| format!("无法启动 {}", executable.display()))
    }

    fn http_get(
        address: Ipv4Addr,
        port: u16,
        path: &str,
        token: Option<&str>,
    ) -> anyhow::Result<Vec<u8>> {
        let socket = SocketAddr::new(IpAddr::V4(address), port);
        let mut stream = TcpStream::connect_timeout(&socket, Duration::from_secs(1))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let authorization = token
            .map(|value| format!("Authorization: Bearer {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {address}:{port}\r\n{authorization}Connection: close\r\n\r\n"
        )?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        let separator = response
            .windows(4)
            .position(|item| item == b"\r\n\r\n")
            .context("HTTP 响应不完整")?;
        let headers = std::str::from_utf8(&response[..separator])?;
        if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
            bail!(
                "HTTP 服务返回非成功状态：{}",
                headers.lines().next().unwrap_or("unknown")
            );
        }
        Ok(response[separator + 4..].to_vec())
    }

    fn wait_for_health(
        address: Ipv4Addr,
        port: u16,
        child: &mut Child,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                bail!("信令服务启动失败（{status}）");
            }
            if http_get(address, port, "/api/healthz", None).is_ok_and(|body| body == b"ok") {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
        bail!("等待信令服务启动超时")
    }

    fn wait_for_tcp_listener(
        address: Ipv4Addr,
        port: u16,
        child: &mut Child,
        name: &str,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                bail!("{name} 服务启动失败（{status}）");
            }
            if TcpStream::connect_timeout(
                &SocketAddr::new(IpAddr::V4(address), port),
                Duration::from_millis(500),
            )
            .is_ok()
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        bail!("等待 {name} 服务启动超时")
    }

    fn wait_for_device(
        address: Ipv4Addr,
        port: u16,
        token: &str,
        child: &mut Child,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                bail!("Host 启动失败（{status}）");
            }
            if let Ok(body) = http_get(address, port, "/api/devices", Some(token))
                && let Ok(Value::Array(devices)) = serde_json::from_slice::<Value>(&body)
                && devices.iter().any(|device| {
                    device.get("id").and_then(Value::as_str) == Some(DEVICE_ID)
                        && device.get("online").and_then(Value::as_bool) == Some(true)
                })
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
        bail!("Host 未能在信令服务中上线")
    }

    fn write_qr_code(path: &Path, value: &str) -> anyhow::Result<()> {
        let code = QrCode::new(value.as_bytes()).context("无法生成二维码")?;
        let image = code.render::<svg::Color>().min_dimensions(480, 480).build();
        fs::write(path, image)?;
        Ok(())
    }

    fn process_image_path(process_id: u32) -> Option<PathBuf> {
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
        let mut buffer = vec![0_u16; 32_768];
        let mut length = buffer.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_FORMAT(0),
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
        unsafe { CloseHandle(process).ok() };
        result.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..length as usize],
        )))
    }

    fn stop_existing_stack(root: &Path, data_dir: &Path) -> anyhow::Result<()> {
        let path = data_dir.join("status.json");
        let Ok(Value::Object(status)) =
            serde_json::from_slice::<Value>(&fs::read(path).unwrap_or_default())
        else {
            return Ok(());
        };
        if let Some(launcher_pid) = status
            .get("launcher_pid")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            let _ = fs::write(
                data_dir.join(format!("shutdown-{launcher_pid}.requested")),
                b"requested\n",
            );
        }
        for (field, name) in [
            ("host_pid", "remote-host.exe"),
            ("signaling_pid", "remote-signaling.exe"),
            ("turn_pid", "remote-turn.exe"),
        ] {
            let Some(pid) = status
                .get(field)
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                continue;
            };
            let expected = root
                .join(name)
                .canonicalize()
                .unwrap_or_else(|_| root.join(name));
            let Some(actual) = process_image_path(pid).and_then(|path| path.canonicalize().ok())
            else {
                continue;
            };
            if actual != expected {
                continue;
            }
            let result = Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if !result.success() && process_image_path(pid).is_some() {
                bail!("无法停止旧进程 {pid}");
            }
        }
        Ok(())
    }

    fn stop_control_panel(root: &Path, data_dir: &Path) -> anyhow::Result<()> {
        let state = fs::read(data_dir.join("panel-state.json")).unwrap_or_default();
        let Ok(Value::Object(state)) = serde_json::from_slice::<Value>(&state) else {
            return Ok(());
        };
        let Some(pid) = state
            .get("panel_pid")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        else {
            return Ok(());
        };
        let expected = root
            .join("remote-control-panel.exe")
            .canonicalize()
            .unwrap_or_else(|_| root.join("remote-control-panel.exe"));
        let Some(actual) = process_image_path(pid).and_then(|path| path.canonicalize().ok()) else {
            return Ok(());
        };
        if actual != expected {
            return Ok(());
        }
        let result = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if !result.success() && process_image_path(pid).is_some() {
            bail!("无法停止控制面板进程 {pid}");
        }
        Ok(())
    }

    fn configure_firewall(root: &Path, udp_port: u16) -> anyhow::Result<()> {
        remove_firewall_rules(root);
        for (name, protocol, ports) in [
            ("Super Remote Web", "TCP", udp_port.to_string()),
            ("Super Remote TURN TCP", "TCP", TURN_TCP_PORT.to_string()),
            ("Super Remote TURN UDP", "UDP", udp_port.to_string()),
            (
                "Super Remote TURN Relay UDP",
                "UDP",
                format!("{TURN_RELAY_MIN_PORT}-{TURN_RELAY_MAX_PORT}"),
            ),
        ] {
            let status = Command::new("netsh.exe")
                .args([
                    "advfirewall",
                    "firewall",
                    "add",
                    "rule",
                    &format!("name={name}"),
                    "dir=in",
                    "action=allow",
                    &format!("protocol={protocol}"),
                    &format!("localport={ports}"),
                    "remoteip=localsubnet",
                    "profile=any",
                    "enable=yes",
                ])
                .current_dir(root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if !status.success() {
                bail!("无法配置防火墙规则：{name}");
            }
        }
        Ok(())
    }

    fn remove_firewall_rules(root: &Path) {
        for name in [
            "Super Remote Host ICE-TCP",
            "Super Remote Web",
            "Super Remote TURN TCP",
            "Super Remote TURN UDP",
            "Super Remote TURN Relay UDP",
        ] {
            let _ = Command::new("netsh.exe")
                .args([
                    "advfirewall",
                    "firewall",
                    "delete",
                    "rule",
                    &format!("name={name}"),
                ])
                .current_dir(root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    }

    fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn unix_seconds() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn show_error(error: &str) {
        let message = HSTRING::from(format!(
            "Super Remote 启动失败：\n\n{error}\n\n请检查 C:\\ProgramData\\Super Remote 下的日志。"
        ));
        unsafe {
            MessageBoxW(None, &message, w!("Super Remote"), MB_OK | MB_ICONERROR);
        }
    }

    #[cfg(test)]
    mod tests {
        use std::ffi::OsStr;

        use super::{lan_ip, quote_argument, stream_dimensions, write_qr_code};

        #[test]
        fn quotes_windows_arguments_with_spaces_and_quotes() {
            assert_eq!(quote_argument(OsStr::new("plain")), "plain");
            assert_eq!(quote_argument(OsStr::new("two words")), "\"two words\"");
            assert_eq!(quote_argument(OsStr::new("a\\\"b")), "\"a\\\\\\\"b\"");
        }

        #[test]
        fn creates_a_self_contained_svg_qr_code() {
            let path = std::env::temp_dir().join(format!(
                "super-remote-qr-test-{}-{}.svg",
                std::process::id(),
                super::unix_seconds()
            ));
            write_qr_code(&path, "http://192.168.1.2:8080/#token=test").unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            assert!(svg.contains("<svg"));
            let _ = std::fs::remove_file(path);
        }

        #[test]
        fn software_video_fallback_preserves_aspect_ratio() {
            assert_eq!(stream_dimensions(2560, 1600, Some(1920)), (1920, 1200));
            assert_eq!(stream_dimensions(1920, 1080, Some(1600)), (1600, 900));
            assert_eq!(stream_dimensions(1280, 800, Some(1600)), (1280, 800));
            assert_eq!(stream_dimensions(2560, 1600, None), (2560, 1600));
        }

        #[test]
        fn only_rfc1918_addresses_are_lan_addresses() {
            assert!(
                "192.168.0.115"
                    .parse::<std::net::Ipv4Addr>()
                    .unwrap()
                    .is_private()
            );
            assert!(
                "10.12.0.5"
                    .parse::<std::net::Ipv4Addr>()
                    .unwrap()
                    .is_private()
            );
            assert!(
                "172.20.1.5"
                    .parse::<std::net::Ipv4Addr>()
                    .unwrap()
                    .is_private()
            );
            assert!(
                !"198.18.0.1"
                    .parse::<std::net::Ipv4Addr>()
                    .unwrap()
                    .is_private()
            );
            assert!(
                !"169.254.83.107"
                    .parse::<std::net::Ipv4Addr>()
                    .unwrap()
                    .is_private()
            );
        }

        #[test]
        #[ignore = "requires a connected Ethernet or Wi-Fi adapter with a default gateway"]
        fn selects_the_physical_lan_instead_of_a_tunnel_route() {
            let address = lan_ip().unwrap();
            eprintln!("selected LAN IPv4: {address}");
            assert!(address.is_private());
            assert_ne!(address, "198.18.0.1".parse::<std::net::Ipv4Addr>().unwrap());
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_launcher::run() {
        windows_launcher::show_error(&format!("{error:#}"));
    }
}
