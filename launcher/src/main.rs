#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[path = "../../host/src/ffmpeg_options.rs"]
mod ffmpeg_options;
#[cfg(windows)]
mod process_tree;

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
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use crate::process_tree::{ExistingProcess, ManagedChild, ServiceJob};
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

    #[derive(Debug)]
    struct ShutdownRequested;

    impl std::fmt::Display for ShutdownRequested {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("服务启动已取消")
        }
    }

    impl std::error::Error for ShutdownRequested {}

    fn check_shutdown(marker: &Path) -> anyhow::Result<()> {
        if marker.is_file() {
            return Err(ShutdownRequested.into());
        }
        Ok(())
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
        let arguments = env::args().collect::<Vec<_>>();
        if arguments
            .get(1)
            .is_some_and(|argument| argument == "--diagnose-video")
        {
            anyhow::ensure!(
                arguments.len() == 4,
                "--diagnose-video requires runtime and output directories"
            );
            let runtime = PathBuf::from(&arguments[2]);
            let directory = PathBuf::from(&arguments[3]);
            anyhow::ensure!(
                runtime.is_absolute() && directory.is_absolute(),
                "diagnostic paths must be absolute"
            );
            fs::create_dir_all(&directory)?;
            let (width, height, _, _) = display_geometry()?;
            let selected = select_video_pipeline(
                &runtime,
                &directory.join("diagnostic-shutdown.requested"),
                width,
                height,
            )?;
            write_json(
                &directory.join("video-diagnostic.json"),
                &serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"), "encoder": selected.encoder,
                    "fps": selected.fps, "capture_mode": selected.capture_mode,
                }),
            )?;
            return Ok(());
        }
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
            let _ = fs::remove_file(
                data_dir.join(format!("shutdown-{}.requested", std::process::id())),
            );
            return Ok(());
        }

        let job = ServiceJob::new().context("无法创建服务进程组")?;
        let shutdown_marker = data_dir.join(format!("shutdown-{}.requested", std::process::id()));
        // Publish the supervisor before startup, so closing the panel during a
        // restart can cancel it even before status.json has new service PIDs.
        write_json(
            &data_dir.join("supervisor-state.json"),
            &serde_json::json!({
                "launcher_pid": std::process::id(),
                "process_tree_managed": true,
            }),
        )?;
        let result = run_services(&root, &data_dir, &job, &shutdown_marker);
        let stopped = job.stop();
        let _ = fs::remove_file(&shutdown_marker);
        if result
            .as_ref()
            .is_err_and(|error| error.is::<ShutdownRequested>())
        {
            return stopped;
        }
        result.and(stopped)
    }

    fn run_services(
        root: &Path,
        data_dir: &Path,
        job: &ServiceJob,
        shutdown_marker: &Path,
    ) -> anyhow::Result<()> {
        check_shutdown(shutdown_marker)?;
        require_runtime_files(&root)?;
        let (primary_width, primary_height, desktop_width, desktop_height) = display_geometry()?;
        let video_pipeline =
            select_video_pipeline(&root, shutdown_marker, primary_width, primary_height)?;
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
        check_shutdown(shutdown_marker)?;

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
            (
                "REMOTE_TURN_TCP_BRIDGE",
                format!("127.0.0.1:{TURN_TCP_PORT}"),
            ),
            ("RUST_LOG", "remote_signaling=info,remote_host=info".into()),
            (
                "SUPER_REMOTE_DATA_DIR",
                data_dir.to_string_lossy().into_owned(),
            ),
        ]);

        let mut turn = spawn_logged(
            job,
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
            shutdown_marker,
        )?;

        let mut signaling = spawn_logged(
            job,
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
            shutdown_marker,
        )?;

        let config_path = data_dir.join("remote-host.toml");
        let mut host = spawn_logged(
            job,
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
            shutdown_marker,
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
        check_shutdown(shutdown_marker)?;

        let mut panel = Command::new(root.join("remote-control-panel.exe"));
        panel
            .arg(&root)
            .env("SUPER_REMOTE_DATA_DIR", &data_dir)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
        let _panel = panel.spawn().context("无法启动控制面板")?;
        let panel = wait_for_control_panel(root, data_dir, shutdown_marker)?;

        loop {
            if shutdown_marker.is_file() || panel.wait(Duration::ZERO)? {
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
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_control_panel(
        root: &Path,
        data_dir: &Path,
        shutdown_marker: &Path,
    ) -> anyhow::Result<ExistingProcess> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            check_shutdown(shutdown_marker)?;
            let state: Value = serde_json::from_slice(
                &fs::read(data_dir.join("panel-state.json")).unwrap_or_default(),
            )
            .unwrap_or_default();
            if let Some(id) = state["panel_pid"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
                && let Some(panel) =
                    ExistingProcess::open(id, &root.join("remote-control-panel.exe"))?
            {
                return Ok(panel);
            }
            thread::sleep(Duration::from_millis(100));
        }
        bail!("控制面板未能启动，后台服务已停止")
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

    fn select_video_pipeline(
        root: &Path,
        shutdown_marker: &Path,
        width: u32,
        height: u32,
    ) -> anyhow::Result<VideoPipeline> {
        let ffmpeg = root.join("ffmpeg.exe");
        require_verified_60fps(|| {
            check_shutdown(shutdown_marker)?;
            let ready = probe_ffmpeg_encoder(&ffmpeg, "h264_nvenc", shutdown_marker)?
                && probe_desktop_pipeline(&ffmpeg, shutdown_marker, width, height)?;
            if !ready {
                // A previous GPU process may still be releasing driver resources.
                // Keep cancellation responsive while giving initialization time.
                for _ in 0..10 {
                    check_shutdown(shutdown_marker)?;
                    thread::sleep(Duration::from_millis(50));
                }
            }
            Ok(ready)
        })
        .with_context(|| {
            format!(
                "无法启动 60 FPS 恒定画质采集，未降级到 30 FPS。请检查 {}",
                shutdown_marker
                    .parent()
                    .unwrap_or(root)
                    .join("encoder-probes.log")
                    .display()
            )
        })
    }

    fn require_verified_60fps(
        mut probe: impl FnMut() -> anyhow::Result<bool>,
    ) -> anyhow::Result<VideoPipeline> {
        for _ in 0..3 {
            if probe()? {
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
        }
        bail!(
            "NVENC / Desktop Duplication 连续三次初始化未通过；60 FPS 模式需要可用的 NVIDIA 编码器"
        )
    }

    fn probe_desktop_pipeline(
        ffmpeg: &Path,
        shutdown_marker: &Path,
        width: u32,
        height: u32,
    ) -> anyhow::Result<bool> {
        // Validate the real capture -> GPU conversion -> constant-QP encoder,
        // not just a CPU black frame. Frames go to the null muxer, never disk.
        let mut arguments = [
            "-hide_banner",
            "-loglevel",
            "info",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "ddagrab=output_idx=0:draw_mouse=1:framerate=67:dup_frames=1",
            "-vf",
        ]
        .map(str::to_owned)
        .to_vec();
        arguments.push(format!(
            "scale_d3d11=width={width}:height={height}:format=nv12,fps=fps=60:round=near"
        ));
        arguments
            .extend(["-frames:v", "120", "-an", "-fps_mode", "passthrough"].map(str::to_owned));
        arguments.extend(crate::ffmpeg_options::encoding_args(
            "h264_nvenc",
            20_000_000,
            60,
        ));
        arguments.extend(["-f", "null", "NUL"].map(str::to_owned));
        run_video_probe(ffmpeg, "ddagrab-nvenc-60fps", shutdown_marker, &arguments)
    }

    fn probe_ffmpeg_encoder(
        ffmpeg: &Path,
        encoder: &str,
        shutdown_marker: &Path,
    ) -> anyhow::Result<bool> {
        let arguments = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=1920x1080:r=60",
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
        ]
        .map(str::to_owned);
        run_video_probe(ffmpeg, encoder, shutdown_marker, &arguments)
    }

    fn run_video_probe(
        ffmpeg: &Path,
        encoder: &str,
        shutdown_marker: &Path,
        arguments: &[String],
    ) -> anyhow::Result<bool> {
        check_shutdown(shutdown_marker)?;
        // Probes also launch FFmpeg. Give each a short-lived job so cancellation
        // or a hung GPU driver cannot leave a startup probe behind either.
        let job = ServiceJob::new()?;
        let log_path = shutdown_marker
            .parent()
            .context("探测日志目录不存在")?
            .join("encoder-probes.log");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        writeln!(
            log,
            "\n[{}] launcher={} version={} encoder={} probe starting",
            unix_seconds(),
            std::process::id(),
            env!("CARGO_PKG_VERSION"),
            encoder
        )?;
        let child = job.spawn(
            ffmpeg,
            arguments,
            ffmpeg.parent().context("FFmpeg 没有父目录")?,
            &log,
            &BTreeMap::new(),
        )?;
        let result = (|| {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                check_shutdown(shutdown_marker)?;
                if let Some(status) = child.try_wait()? {
                    writeln!(log, "encoder={encoder} exit={status}")?;
                    return Ok(status.success());
                }
                if Instant::now() >= deadline {
                    writeln!(log, "encoder={encoder} timed out")?;
                    return Ok(false);
                }
                thread::sleep(Duration::from_millis(50));
            }
        })();
        job.stop()?;
        result
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
        job: &ServiceJob,
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
        job.spawn(executable, arguments, root, &stdout, environment)
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
        child: &mut ManagedChild,
        timeout: Duration,
        shutdown_marker: &Path,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            check_shutdown(shutdown_marker)?;
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
        child: &mut ManagedChild,
        name: &str,
        timeout: Duration,
        shutdown_marker: &Path,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            check_shutdown(shutdown_marker)?;
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
        child: &mut ManagedChild,
        timeout: Duration,
        shutdown_marker: &Path,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            check_shutdown(shutdown_marker)?;
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
        let status: Value =
            serde_json::from_slice(&fs::read(data_dir.join("status.json")).unwrap_or_default())
                .unwrap_or_default();
        let supervisor: Value = serde_json::from_slice(
            &fs::read(data_dir.join("supervisor-state.json")).unwrap_or_default(),
        )
        .unwrap_or_default();
        let pid = |state: &Value, field: &str| {
            state
                .get(field)
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .filter(|id| *id != std::process::id())
                .unwrap_or(0)
        };
        let expected_launcher = root.join("super-remote.exe");
        let current = ExistingProcess::open(pid(&supervisor, "launcher_pid"), &expected_launcher)?;
        let managed = current.is_some() && supervisor["process_tree_managed"] == true;
        let launcher = match current {
            Some(process) => Some(process),
            None => ExistingProcess::open(pid(&status, "launcher_pid"), &expected_launcher)?,
        };
        let mut services = Vec::new();
        for (field, name) in [
            ("host_pid", "remote-host.exe"),
            ("signaling_pid", "remote-signaling.exe"),
            ("turn_pid", "remote-turn.exe"),
        ] {
            if let Some(process) = ExistingProcess::open(pid(&status, field), &root.join(name))? {
                services.push(process);
            }
        }
        if managed && let Some(launcher) = &launcher {
            fs::write(
                data_dir.join(format!("shutdown-{}.requested", launcher.id)),
                b"requested\n",
            )
            .context("无法提交服务停止请求")?;
            if !launcher.wait(Duration::from_secs(30))? {
                // Kill only the supervisor, not its UI child. Closing its job
                // handle makes Windows terminate all services and descendants.
                launcher.terminate()?;
            }
        }
        // Legacy installations have no job. Stop Host's tree BEFORE allowing
        // their supervisor to exit, otherwise FFmpeg can lose its parent first.
        for process in services {
            if process.wait(Duration::ZERO)? {
                continue;
            }
            Command::new("taskkill.exe")
                .args(["/PID", &process.id.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if !process.wait(Duration::from_secs(10))? {
                bail!("无法停止旧进程 {}", process.id);
            }
        }
        if let Some(launcher) = launcher {
            launcher.terminate()?;
            let _ = fs::remove_file(data_dir.join(format!("shutdown-{}.requested", launcher.id)));
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
        #[test]
        fn stopping_stack_waits_for_all_processes_but_keeps_panel_for_restart() {
            use super::*;
            use crate::process_tree::tests::{TestDirectory, start_test_stack};
            let directory = TestDirectory::new();
            for _ in 0..2 {
                let (mut owner, mut panel) = start_test_stack(&directory.0);
                let status: Value =
                    serde_json::from_slice(&fs::read(directory.0.join("status.json")).unwrap())
                        .unwrap();
                let mut processes = Vec::new();
                for (key, name) in [
                    ("host_pid", "remote-host.exe"),
                    ("signaling_pid", "remote-signaling.exe"),
                    ("turn_pid", "remote-turn.exe"),
                ] {
                    processes.push(
                        ExistingProcess::open(
                            status[key].as_u64().unwrap() as u32,
                            &directory.0.join(name),
                        )
                        .unwrap()
                        .unwrap(),
                    );
                }
                let descendant: u32 = fs::read_to_string(directory.0.join("descendant.pid"))
                    .unwrap()
                    .parse()
                    .unwrap();
                let descendant =
                    ExistingProcess::open(descendant, &directory.0.join("remote-host.exe"))
                        .unwrap()
                        .unwrap();
                stop_existing_stack(&directory.0, &directory.0).unwrap();
                assert!(owner.0.try_wait().unwrap().is_some());
                assert!(
                    panel.0.try_wait().unwrap().is_none(),
                    "Stop must preserve the control panel"
                );
                for process in processes {
                    assert!(process.wait(Duration::ZERO).unwrap());
                }
                assert!(descendant.wait(Duration::ZERO).unwrap());
                // A repeated Stop and a later Start on the same data directory
                // must both work with stale status/PID files from the last run.
                stop_existing_stack(&directory.0, &directory.0).unwrap();
                fs::remove_file(directory.0.join("descendant.pid")).unwrap();
            }
        }

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
        #[ignore = "requires bundled FFmpeg and an NVIDIA GPU; encodes synthetic frames only"]
        fn real_nvenc_probe_uses_the_service_job() {
            use super::{PathBuf, env, fs, probe_ffmpeg_encoder};
            let root = PathBuf::from(env::var("SUPER_REMOTE_TEST_RUNTIME").expect("runtime path"));
            let directory = PathBuf::from(
                env::var("SUPER_REMOTE_TEST_DIAGNOSTICS").expect("diagnostic directory"),
            );
            fs::create_dir_all(&directory).unwrap();
            let marker = directory.join("test-shutdown.requested");
            let result =
                probe_ffmpeg_encoder(&root.join("ffmpeg.exe"), "h264_nvenc", &marker).unwrap();
            eprintln!(
                "{}",
                fs::read_to_string(directory.join("encoder-probes.log")).unwrap()
            );
            assert!(
                result,
                "NVENC failed inside the launcher's real process job"
            );
        }

        #[test]
        fn sixty_fps_selection_retries_transient_failures_without_lowering_quality() {
            let mut attempts = 0;
            let pipeline = super::require_verified_60fps(|| {
                attempts += 1;
                Ok(attempts == 3)
            })
            .unwrap();
            assert_eq!(attempts, 3);
            assert_eq!(pipeline.fps, 60);
            assert_eq!(pipeline.encoder, "h264_nvenc");
            assert_eq!(pipeline.capture_mode, "ddagrab");
            assert_eq!(pipeline.max_width, None);
            assert_eq!(crate::ffmpeg_options::fixed_qp(pipeline.encoder), Some(18));
        }

        #[test]
        fn sixty_fps_selection_never_silently_returns_a_thirty_fps_pipeline() {
            let mut attempts = 0;
            let result = super::require_verified_60fps(|| {
                attempts += 1;
                Ok(false)
            });
            assert!(result.is_err());
            assert_eq!(attempts, 3);
        }

        #[test]
        fn sixty_fps_selection_does_not_retry_shutdown() {
            let mut attempts = 0;
            let result = super::require_verified_60fps(|| {
                attempts += 1;
                Err(super::ShutdownRequested.into())
            });
            assert!(result.err().unwrap().is::<super::ShutdownRequested>());
            assert_eq!(attempts, 1);
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
        if std::env::args().any(|argument| argument == "--stop") {
            // The control panel owns error presentation. Do not strand a hidden
            // stop helper behind a modal dialog or report failure as exit code 0.
            eprintln!("停止服务失败：{error:#}");
        } else {
            windows_launcher::show_error(&format!("{error:#}"));
        }
        std::process::exit(1);
    }
}
