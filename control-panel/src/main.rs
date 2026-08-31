#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("remote-control-panel is only available on Windows");
}

#[cfg(windows)]
mod windows_app {
    use std::{
        ffi::c_void,
        fs::{self, File, OpenOptions},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::atomic::{AtomicBool, AtomicIsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde::{Deserialize, Serialize};
    use windows::{
        Win32::{
            Foundation::{
                COLORREF, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM,
                LRESULT, WAIT_TIMEOUT, WPARAM,
            },
            Graphics::Gdi::{
                BLACK_BRUSH, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH,
                FF_DONTCARE, FW_NORMAL, GetStockObject, HBRUSH, HGDIOBJ, PROOF_QUALITY,
            },
            Media::Audio::{
                Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole,
                eRender,
            },
            System::{
                Com::{
                    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                    CoUninitialize,
                },
                LibraryLoader::GetModuleHandleW,
                Threading::{
                    CREATE_NEW_PROCESS_GROUP, CreateMutexW, DETACHED_PROCESS, OpenProcess,
                    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
                },
            },
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
                    SetProcessDpiAwarenessContext,
                },
                Shell::ShellExecuteW,
                WindowsAndMessaging::{
                    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_PUSHBUTTON,
                    CREATESTRUCTW, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                    DispatchMessageW, FindWindowW, GWLP_USERDATA, GetMessageW, GetSystemMetrics,
                    GetWindowLongPtrW, HHOOK, HMENU, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW,
                    KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED,
                    LLMHF_LOWER_IL_INJECTED, LWA_ALPHA, LoadCursorW, MB_ICONERROR, MB_OK, MSG,
                    MSLLHOOKSTRUCT, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
                    SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
                    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOW, SW_SHOWNA,
                    SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_SHOWWINDOW, SendMessageW,
                    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer,
                    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
                    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx,
                    WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE,
                    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE,
                    WM_NCHITTEST, WM_SETFONT, WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_LAYERED,
                    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
                    WS_OVERLAPPEDWINDOW, WS_POPUP, WS_TABSTOP, WS_VISIBLE,
                },
            },
        },
        core::{HSTRING, PCWSTR, w},
    };

    const PANEL_CLASS: PCWSTR = w!("SuperRemoteControlPanel");
    const OVERLAY_CLASS: PCWSTR = w!("SuperRemotePrivacyOverlay");
    const PANEL_TITLE: PCWSTR = w!("Super Remote 控制面板");
    const MUTEX_NAME: PCWSTR = w!("Local\\SuperRemoteControlPanel-822272a");

    const ID_START: usize = 101;
    const ID_STOP: usize = 102;
    const ID_RESTART: usize = 103;
    const ID_OPEN_WEB: usize = 104;
    const ID_OPEN_QR: usize = 105;
    const ID_PRIVACY: usize = 106;
    const ID_HOST_MUTE: usize = 107;
    const WM_LOCAL_PHYSICAL_INPUT: u32 = WM_APP + 1;

    static LOCAL_INPUT_WINDOW: AtomicIsize = AtomicIsize::new(0);
    static LOCAL_INPUT_ARMED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Default, Deserialize)]
    struct LauncherStatus {
        url: String,
        qr: String,
        host_pid: u32,
        signaling_pid: u32,
        primary_display: String,
        encoder: String,
        capture_mode: String,
        #[serde(default)]
        elevated: bool,
        #[serde(default)]
        python_executable: String,
    }

    #[derive(Clone, Default, Deserialize)]
    struct HostStatus {
        host_pid: u32,
        online: bool,
        connection_state: String,
        capture_active: bool,
        width: u32,
        height: u32,
        fps: u16,
        bitrate: u32,
        encoder: String,
        monitor_index: usize,
    }

    #[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
    struct PanelSettings {
        #[serde(default)]
        privacy_screen_on_connect: bool,
        #[serde(default)]
        mute_host_audio_on_connect: bool,
    }

    #[derive(Serialize)]
    struct PanelRuntimeState {
        panel_pid: u32,
        services_running: bool,
        client_connected: bool,
        privacy_requested: bool,
        privacy_supported: bool,
        privacy_overlay_visible: bool,
        privacy_waiting_for_local_input: bool,
        privacy_overlay_bounds: [i32; 4],
        host_audio_mute_requested: bool,
        host_audio_muted: bool,
        updated_at_unix_ms: u128,
    }

    struct AudioMuteLease {
        endpoint: IAudioEndpointVolume,
    }

    struct LocalInputHooks {
        keyboard: HHOOK,
        mouse: HHOOK,
    }

    impl LocalInputHooks {
        fn install(window: HWND, instance: HINSTANCE) -> Result<Self, String> {
            LOCAL_INPUT_WINDOW.store(window.0 as isize, Ordering::Release);
            let keyboard = unsafe {
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), Some(instance), 0)
            }
            .map_err(|error| format!("failed to install local keyboard hook: {error}"))?;
            let mouse = match unsafe {
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), Some(instance), 0)
            } {
                Ok(mouse) => mouse,
                Err(error) => {
                    unsafe { UnhookWindowsHookEx(keyboard).ok() };
                    LOCAL_INPUT_WINDOW.store(0, Ordering::Release);
                    return Err(format!("failed to install local mouse hook: {error}"));
                }
            };
            Ok(Self { keyboard, mouse })
        }
    }

    impl Drop for LocalInputHooks {
        fn drop(&mut self) {
            LOCAL_INPUT_ARMED.store(false, Ordering::Release);
            LOCAL_INPUT_WINDOW.store(0, Ordering::Release);
            unsafe {
                UnhookWindowsHookEx(self.keyboard).ok();
                UnhookWindowsHookEx(self.mouse).ok();
            }
        }
    }

    struct App {
        root: PathBuf,
        run_dir: PathBuf,
        window: HWND,
        service_label: HWND,
        client_label: HWND,
        video_label: HWND,
        address_label: HWND,
        policy_label: HWND,
        audio_policy_label: HWND,
        action_label: HWND,
        privacy_checkbox: HWND,
        host_mute_checkbox: HWND,
        overlay: HWND,
        privacy_supported: bool,
        privacy_visible: bool,
        privacy_latched: bool,
        client_connected: bool,
        audio_mute: Option<AudioMuteLease>,
        settings: PanelSettings,
    }

    impl App {
        fn empty(root: PathBuf) -> Self {
            let run_dir = root.join(".run");
            Self {
                root,
                run_dir,
                window: HWND::default(),
                service_label: HWND::default(),
                client_label: HWND::default(),
                video_label: HWND::default(),
                address_label: HWND::default(),
                policy_label: HWND::default(),
                audio_policy_label: HWND::default(),
                action_label: HWND::default(),
                privacy_checkbox: HWND::default(),
                host_mute_checkbox: HWND::default(),
                overlay: HWND::default(),
                privacy_supported: false,
                privacy_visible: false,
                privacy_latched: false,
                client_connected: false,
                audio_mute: None,
                settings: PanelSettings::default(),
            }
        }

        unsafe fn create_controls(&mut self, instance: HINSTANCE) -> windows::core::Result<()> {
            let dpi = unsafe { GetDpiForWindow(self.window) } as i32;
            let scale = |value: i32| value * dpi / 96;
            let font = unsafe {
                CreateFontW(
                    scale(-18),
                    0,
                    0,
                    0,
                    FW_NORMAL.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    Default::default(),
                    Default::default(),
                    PROOF_QUALITY,
                    u32::from(DEFAULT_PITCH.0 | FF_DONTCARE.0),
                    w!("Microsoft YaHei UI"),
                )
            };
            let title_font = unsafe {
                CreateFontW(
                    scale(-26),
                    0,
                    0,
                    0,
                    600,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    Default::default(),
                    Default::default(),
                    PROOF_QUALITY,
                    u32::from(DEFAULT_PITCH.0 | FF_DONTCARE.0),
                    w!("Microsoft YaHei UI"),
                )
            };
            let title = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("Super Remote"),
                    scale(28),
                    scale(22),
                    scale(540),
                    scale(42),
                    0,
                    0,
                )?
            };
            unsafe { set_font(title, title_font.into()) };

            self.service_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("服务状态：正在读取…"),
                    scale(30),
                    scale(78),
                    scale(540),
                    scale(30),
                    0,
                    0,
                )?
            };
            self.client_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("Web 客户端：—"),
                    scale(30),
                    scale(112),
                    scale(540),
                    scale(30),
                    0,
                    0,
                )?
            };
            self.video_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("视频：—"),
                    scale(30),
                    scale(146),
                    scale(540),
                    scale(30),
                    0,
                    0,
                )?
            };
            self.address_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("访问地址：—"),
                    scale(30),
                    scale(180),
                    scale(540),
                    scale(30),
                    0,
                    0,
                )?
            };

            unsafe {
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("启动"),
                    scale(30),
                    scale(228),
                    scale(104),
                    scale(38),
                    BS_PUSHBUTTON as u32,
                    ID_START,
                )?;
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("停止"),
                    scale(144),
                    scale(228),
                    scale(104),
                    scale(38),
                    BS_PUSHBUTTON as u32,
                    ID_STOP,
                )?;
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("重启"),
                    scale(258),
                    scale(228),
                    scale(104),
                    scale(38),
                    BS_PUSHBUTTON as u32,
                    ID_RESTART,
                )?;
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("打开网页"),
                    scale(372),
                    scale(228),
                    scale(104),
                    scale(38),
                    BS_PUSHBUTTON as u32,
                    ID_OPEN_WEB,
                )?;
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("查看二维码"),
                    scale(486),
                    scale(228),
                    scale(104),
                    scale(38),
                    BS_PUSHBUTTON as u32,
                    ID_OPEN_QR,
                )?;
            }

            self.privacy_checkbox = unsafe {
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("Web 客户端连接后启用本机隐私黑屏"),
                    scale(30),
                    scale(298),
                    scale(550),
                    scale(32),
                    BS_AUTOCHECKBOX as u32,
                    ID_PRIVACY,
                )?
            };
            self.policy_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("所有显示器变黑；断线后须使用本机键盘或鼠标解除。"),
                    scale(52),
                    scale(334),
                    scale(520),
                    scale(32),
                    0,
                    0,
                )?
            };
            self.host_mute_checkbox = unsafe {
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("Web 客户端连接后静音主机声音"),
                    scale(30),
                    scale(376),
                    scale(550),
                    scale(32),
                    BS_AUTOCHECKBOX as u32,
                    ID_HOST_MUTE,
                )?
            };
            self.audio_policy_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("静音本机默认播放设备；客户端断开后仍保持静音。"),
                    scale(52),
                    scale(412),
                    scale(520),
                    scale(32),
                    0,
                    0,
                )?
            };
            self.action_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!(""),
                    scale(30),
                    scale(466),
                    scale(550),
                    scale(32),
                    0,
                    0,
                )?
            };

            for control in [
                self.service_label,
                self.client_label,
                self.video_label,
                self.address_label,
                self.policy_label,
                self.audio_policy_label,
                self.action_label,
                self.privacy_checkbox,
                self.host_mute_checkbox,
            ] {
                unsafe { set_font(control, font.into()) };
            }

            self.settings =
                read_json(&self.run_dir.join("control-settings.json")).unwrap_or_default();
            unsafe {
                SendMessageW(
                    self.privacy_checkbox,
                    BM_SETCHECK,
                    Some(WPARAM(usize::from(self.settings.privacy_screen_on_connect))),
                    None,
                );
                SendMessageW(
                    self.host_mute_checkbox,
                    BM_SETCHECK,
                    Some(WPARAM(usize::from(
                        self.settings.mute_host_audio_on_connect,
                    ))),
                    None,
                );
            }
            Ok(())
        }

        unsafe fn create_overlay(&mut self, instance: HINSTANCE) -> windows::core::Result<()> {
            let [x, y, width, height] = virtual_screen_bounds();
            self.overlay = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE
                        | WS_EX_LAYERED
                        | WS_EX_TRANSPARENT,
                    OVERLAY_CLASS,
                    w!("Super Remote Privacy Screen"),
                    WS_POPUP,
                    x,
                    y,
                    width,
                    height,
                    None,
                    None,
                    Some(instance),
                    None,
                )?
            };
            unsafe { SetLayeredWindowAttributes(self.overlay, COLORREF(0), 255, LWA_ALPHA) }?;
            self.privacy_supported =
                unsafe { SetWindowDisplayAffinity(self.overlay, WDA_EXCLUDEFROMCAPTURE) }.is_ok();
            if !self.privacy_supported {
                set_text(
                    self.policy_label,
                    "当前 Windows/DWM 不支持安全隐私黑屏；为避免远端黑屏，开关不可用。",
                );
            }
            Ok(())
        }

        fn refresh(&mut self) {
            let launcher: LauncherStatus =
                read_json(&self.run_dir.join("status.json")).unwrap_or_default();
            let host: HostStatus =
                read_json(&self.run_dir.join("host-state.json")).unwrap_or_default();
            let host_running = process_running(launcher.host_pid);
            let signaling_running = process_running(launcher.signaling_pid);
            let services_running = host_running && signaling_running;
            let host_fresh = host.host_pid == launcher.host_pid && host.online;
            let connected = services_running
                && host_fresh
                && host.connection_state == "connected"
                && host.capture_active;
            self.client_connected = connected;

            set_text(
                self.service_label,
                &format!(
                    "服务：{} · Host {} · Signaling {}{}",
                    if services_running && host_fresh {
                        "运行中"
                    } else {
                        "已停止"
                    },
                    launcher.host_pid,
                    launcher.signaling_pid,
                    if launcher.elevated {
                        " · 管理员"
                    } else {
                        ""
                    }
                ),
            );
            set_text(
                self.client_label,
                &format!(
                    "Web 客户端：{}",
                    if connected {
                        "已连接，正在捕获"
                    } else if services_running {
                        "未连接，编码器空闲"
                    } else {
                        "—"
                    }
                ),
            );
            let (width, height, fps, bitrate, encoder, monitor_index) = if host_fresh {
                (
                    host.width,
                    host.height,
                    host.fps,
                    host.bitrate,
                    host.encoder.as_str(),
                    host.monitor_index,
                )
            } else {
                (0, 0, 0, 0, launcher.encoder.as_str(), 0)
            };
            set_text(
                self.video_label,
                &format!(
                    "视频：{}x{} · {} FPS · {:.1} Mbps · {} · 主屏 {}",
                    width,
                    height,
                    fps,
                    bitrate as f64 / 1_000_000.0,
                    encoder,
                    monitor_index + 1
                ),
            );
            set_text(
                self.address_label,
                &format!(
                    "地址：{} · 主屏 {} · {}",
                    launcher.url, launcher.primary_display, launcher.capture_mode
                ),
            );

            let disk_settings: PanelSettings =
                read_json(&self.run_dir.join("control-settings.json")).unwrap_or_default();
            if disk_settings != self.settings {
                self.settings = disk_settings;
                unsafe {
                    SendMessageW(
                        self.privacy_checkbox,
                        BM_SETCHECK,
                        Some(WPARAM(usize::from(self.settings.privacy_screen_on_connect))),
                        None,
                    );
                    SendMessageW(
                        self.host_mute_checkbox,
                        BM_SETCHECK,
                        Some(WPARAM(usize::from(
                            self.settings.mute_host_audio_on_connect,
                        ))),
                        None,
                    );
                }
            }
            self.privacy_latched = next_privacy_latch(
                self.privacy_latched,
                connected,
                self.privacy_supported,
                self.settings.privacy_screen_on_connect,
            );
            let should_show = self.privacy_latched
                && self.privacy_supported
                && self.settings.privacy_screen_on_connect;
            self.set_privacy_visible(should_show);
            LOCAL_INPUT_ARMED.store(should_show && !connected, Ordering::Release);
            self.set_host_audio_muted(connected && self.settings.mute_host_audio_on_connect);
            self.write_runtime_state(services_running, connected);
        }

        fn set_privacy_visible(&mut self, visible: bool) {
            if self.overlay.0.is_null() {
                return;
            }
            if self.privacy_visible != visible {
                unsafe {
                    if visible {
                        let [x, y, width, height] = virtual_screen_bounds();
                        let _ = SetWindowPos(
                            self.overlay,
                            Some(HWND_TOPMOST),
                            x,
                            y,
                            width,
                            height,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        let _ = ShowWindow(self.overlay, SW_SHOWNA);
                    } else {
                        let _ = ShowWindow(self.overlay, SW_HIDE);
                    }
                }
                self.privacy_visible = visible;
            }
            set_text(
                self.action_label,
                if visible {
                    if self.client_connected {
                        "所有显示器隐私黑屏已启用；断线后需使用本机键盘或鼠标解除。"
                    } else {
                        "客户端已断开；正在等待本机键盘或鼠标操作解除隐私黑屏。"
                    }
                } else {
                    ""
                },
            );
        }

        fn release_privacy_from_local_input(&mut self) {
            LOCAL_INPUT_ARMED.store(false, Ordering::Release);
            if !self.privacy_latched || self.client_connected {
                return;
            }
            self.privacy_latched = false;
            self.set_privacy_visible(false);
            let launcher: LauncherStatus =
                read_json(&self.run_dir.join("status.json")).unwrap_or_default();
            self.write_runtime_state(
                process_running(launcher.host_pid) && process_running(launcher.signaling_pid),
                false,
            );
        }

        fn toggle_privacy_setting(&mut self) {
            if !self.privacy_supported {
                unsafe {
                    SendMessageW(self.privacy_checkbox, BM_SETCHECK, Some(WPARAM(0)), None);
                }
                return;
            }
            let checked =
                unsafe { SendMessageW(self.privacy_checkbox, BM_GETCHECK, None, None).0 == 1 };
            self.settings.privacy_screen_on_connect = checked;
            let _ = write_json(&self.run_dir.join("control-settings.json"), &self.settings);
            self.refresh();
        }

        fn toggle_host_mute_setting(&mut self) {
            let checked =
                unsafe { SendMessageW(self.host_mute_checkbox, BM_GETCHECK, None, None).0 == 1 };
            self.settings.mute_host_audio_on_connect = checked;
            let _ = write_json(&self.run_dir.join("control-settings.json"), &self.settings);
            self.refresh();
        }

        fn set_host_audio_muted(&mut self, muted: bool) {
            if muted {
                if let Some(lease) = &self.audio_mute {
                    let _ = unsafe { lease.endpoint.SetMute(true, std::ptr::null()) };
                    return;
                }
                match default_audio_endpoint_volume() {
                    Ok(endpoint) => match unsafe { endpoint.SetMute(true, std::ptr::null()) } {
                        Ok(()) => {
                            self.audio_mute = Some(AudioMuteLease { endpoint });
                        }
                        Err(error) => {
                            set_text(self.action_label, &format!("无法静音主机声音：{error}"))
                        }
                    },
                    Err(error) => set_text(
                        self.action_label,
                        &format!("找不到可静音的默认播放设备：{error}"),
                    ),
                }
            } else {
                // Releasing the policy lease deliberately does not unmute the
                // endpoint. Disconnecting a remote client must leave the Host
                // silent until the user explicitly unmutes it in Windows.
                self.audio_mute.take();
            }
        }

        fn action(&self, kind: Action) {
            let launcher: LauncherStatus =
                read_json(&self.run_dir.join("status.json")).unwrap_or_default();
            match kind {
                Action::OpenWeb if !launcher.url.is_empty() => {
                    shell_open(&launcher.url, &self.root);
                }
                Action::OpenQr if !launcher.qr.is_empty() => {
                    shell_open(&launcher.qr, &self.root);
                }
                Action::Start | Action::Stop | Action::Restart => {
                    let python = if launcher.python_executable.is_empty() {
                        "python".into()
                    } else {
                        launcher.python_executable
                    };
                    let mut arguments = Vec::new();
                    if matches!(kind, Action::Stop) {
                        arguments.push("--stop");
                    }
                    spawn_launcher(&self.root, &python, &arguments);
                    set_text(
                        self.action_label,
                        match kind {
                            Action::Start => "启动命令已提交…",
                            Action::Stop => "停止命令已提交…",
                            Action::Restart => "重启命令已提交…",
                            _ => "",
                        },
                    );
                }
                _ => {}
            }
        }

        fn write_runtime_state(&self, services_running: bool, client_connected: bool) {
            let state = PanelRuntimeState {
                panel_pid: std::process::id(),
                services_running,
                client_connected,
                privacy_requested: self.settings.privacy_screen_on_connect,
                privacy_supported: self.privacy_supported,
                privacy_overlay_visible: self.privacy_visible,
                privacy_waiting_for_local_input: self.privacy_visible && !client_connected,
                privacy_overlay_bounds: virtual_screen_bounds(),
                host_audio_mute_requested: self.settings.mute_host_audio_on_connect,
                host_audio_muted: default_audio_endpoint_is_muted(),
                updated_at_unix_ms: now_ms(),
            };
            let _ = write_json(&self.run_dir.join("panel-state.json"), &state);
        }
    }

    #[derive(Clone, Copy)]
    enum Action {
        Start,
        Stop,
        Restart,
        OpenWeb,
        OpenQr,
    }

    pub fn run() -> Result<(), String> {
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .map_err(|error| error.to_string())?;
        let root = root_path();
        if handle_command(&root)? {
            return Ok(());
        }

        let mutex =
            unsafe { CreateMutexW(None, false, MUTEX_NAME) }.map_err(|error| error.to_string())?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = unsafe { FindWindowW(PANEL_CLASS, PANEL_TITLE) } {
                unsafe {
                    let _ = ShowWindow(existing, SW_SHOW);
                    let _ = SetForegroundWindow(existing);
                }
            }
            unsafe { CloseHandle(mutex).ok() };
            return Ok(());
        }

        let module = unsafe { GetModuleHandleW(None) }.map_err(|error| error.to_string())?;
        let instance = HINSTANCE(module.0);
        register_classes(instance)?;
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|error| format!("failed to initialize COM: {error}"))?;

        let mut app = Box::new(App::empty(root));
        let app_ptr = (&mut *app) as *mut App;
        let dpi = unsafe { GetDpiForSystem() } as i32;
        let scale = |value: i32| value * dpi / 96;
        let panel_width = scale(640);
        let panel_height = scale(570);
        let panel_x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - panel_width) / 2;
        let panel_y = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - panel_height) / 2;
        app.window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PANEL_CLASS,
                PANEL_TITLE,
                WS_OVERLAPPEDWINDOW & !WINDOW_STYLE(0x0001_0000),
                panel_x,
                panel_y,
                panel_width,
                panel_height,
                None,
                None,
                Some(instance),
                Some(app_ptr.cast()),
            )
        }
        .map_err(|error| error.to_string())?;
        unsafe {
            app.create_controls(instance)
                .map_err(|error| error.to_string())?;
            app.create_overlay(instance)
                .map_err(|error| error.to_string())?;
            let _local_input_hooks = LocalInputHooks::install(app.window, instance)?;
            let _ = ShowWindow(app.window, SW_SHOWNORMAL);
            SetTimer(Some(app.window), 1, 500, None);
            app.refresh();

            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        app.set_privacy_visible(false);
        app.set_host_audio_muted(false);
        unsafe { CloseHandle(mutex).ok() };
        unsafe { CoUninitialize() };
        Ok(())
    }

    fn handle_command(root: &Path) -> Result<bool, String> {
        let arguments = std::env::args().skip(1).collect::<Vec<_>>();
        let (field, value) = match arguments.as_slice() {
            [flag, value] if flag == "--set-privacy" || flag == "--set-host-mute" => {
                let parsed = match value.as_str() {
                    "true" | "1" | "on" => true,
                    "false" | "0" | "off" => false,
                    _ => return Err(format!("{flag} expects true or false")),
                };
                (Some(flag.as_str()), Some(parsed))
            }
            _ => (None, None),
        };
        let (Some(field), Some(value)) = (field, value) else {
            return Ok(false);
        };
        let mut settings: PanelSettings =
            read_json(&root.join(".run/control-settings.json")).unwrap_or_default();
        if field == "--set-privacy" {
            settings.privacy_screen_on_connect = value;
        } else {
            settings.mute_host_audio_on_connect = value;
        }
        write_json(&root.join(".run/control-settings.json"), &settings)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    fn root_path() -> PathBuf {
        if let Some(argument) = std::env::args().nth(1)
            && !argument.starts_with("--")
        {
            return PathBuf::from(argument);
        }
        std::env::current_exe()
            .ok()
            .and_then(|path| path.parent()?.parent()?.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn next_privacy_latch(
        currently_latched: bool,
        client_connected: bool,
        privacy_supported: bool,
        privacy_enabled: bool,
    ) -> bool {
        if !privacy_supported || !privacy_enabled {
            false
        } else if client_connected {
            true
        } else {
            currently_latched
        }
    }

    fn keyboard_input_is_physical(event: &KBDLLHOOKSTRUCT) -> bool {
        !event.flags.contains(LLKHF_INJECTED) && !event.flags.contains(LLKHF_LOWER_IL_INJECTED)
    }

    fn mouse_input_is_physical(event: &MSLLHOOKSTRUCT) -> bool {
        event.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) == 0
    }

    fn notify_local_physical_input() {
        if !LOCAL_INPUT_ARMED.swap(false, Ordering::AcqRel) {
            return;
        }
        let window = LOCAL_INPUT_WINDOW.load(Ordering::Acquire);
        if window == 0 {
            return;
        }
        let result = unsafe {
            PostMessageW(
                Some(HWND(window as *mut c_void)),
                WM_LOCAL_PHYSICAL_INPUT,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if result.is_err() {
            LOCAL_INPUT_ARMED.store(true, Ordering::Release);
        }
    }

    unsafe extern "system" fn keyboard_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if keyboard_input_is_physical(event) {
                notify_local_physical_input();
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    unsafe extern "system" fn mouse_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let event = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if mouse_input_is_physical(event) {
                notify_local_physical_input();
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    fn register_classes(instance: HINSTANCE) -> Result<(), String> {
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.map_err(|error| error.to_string())?;
        let panel_brush = unsafe { CreateSolidBrush(COLORREF(0x00f7f7f7)) };
        let black = unsafe { GetStockObject(BLACK_BRUSH) };
        let panel = WNDCLASSW {
            hCursor: cursor,
            hInstance: instance,
            lpszClassName: PANEL_CLASS,
            lpfnWndProc: Some(panel_window_proc),
            hbrBackground: panel_brush,
            ..Default::default()
        };
        let overlay = WNDCLASSW {
            hCursor: cursor,
            hInstance: instance,
            lpszClassName: OVERLAY_CLASS,
            lpfnWndProc: Some(overlay_window_proc),
            hbrBackground: HBRUSH(black.0),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&panel) } == 0 || unsafe { RegisterClassW(&overlay) } == 0 {
            return Err("failed to register control panel window classes".into());
        }
        Ok(())
    }

    unsafe extern "system" fn panel_window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize) };
        }
        let app_ptr = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut App };
        match message {
            WM_CREATE => LRESULT(0),
            WM_TIMER if !app_ptr.is_null() => {
                unsafe { (&mut *app_ptr).refresh() };
                LRESULT(0)
            }
            WM_LOCAL_PHYSICAL_INPUT if !app_ptr.is_null() => {
                unsafe { (&mut *app_ptr).release_privacy_from_local_input() };
                LRESULT(0)
            }
            WM_COMMAND if !app_ptr.is_null() => {
                let id = wparam.0 & 0xffff;
                let notification = (wparam.0 >> 16) & 0xffff;
                if notification == BN_CLICKED as usize {
                    let app = unsafe { &mut *app_ptr };
                    match id {
                        ID_START => app.action(Action::Start),
                        ID_STOP => app.action(Action::Stop),
                        ID_RESTART => app.action(Action::Restart),
                        ID_OPEN_WEB => app.action(Action::OpenWeb),
                        ID_OPEN_QR => app.action(Action::OpenQr),
                        ID_PRIVACY => app.toggle_privacy_setting(),
                        ID_HOST_MUTE => app.toggle_host_mute_setting(),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = unsafe { DestroyWindow(window) };
                LRESULT(0)
            }
            WM_DESTROY => {
                if !app_ptr.is_null() {
                    unsafe {
                        (&mut *app_ptr).set_privacy_visible(false);
                        (&mut *app_ptr).set_host_audio_muted(false);
                    };
                }
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }

    unsafe extern "system" fn overlay_window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCHITTEST {
            return LRESULT(HTTRANSPARENT as isize);
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn child(
        parent: HWND,
        instance: HINSTANCE,
        class: PCWSTR,
        text: PCWSTR,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        control_style: u32,
        id: usize,
    ) -> windows::core::Result<HWND> {
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                text,
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(control_style),
                x,
                y,
                width,
                height,
                Some(parent),
                if id == 0 {
                    None
                } else {
                    Some(HMENU(id as *mut c_void))
                },
                Some(instance),
                None,
            )
        }
    }

    unsafe fn set_font(window: HWND, font: HGDIOBJ) {
        unsafe {
            SendMessageW(
                window,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }

    fn set_text(window: HWND, text: &str) {
        if window.0.is_null() {
            return;
        }
        let text = HSTRING::from(text);
        let _ = unsafe { SetWindowTextW(window, &text) };
    }

    fn virtual_screen_bounds() -> [i32; 4] {
        [
            unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
            unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
            unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) },
            unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) },
        ]
    }

    fn default_audio_endpoint_volume() -> windows::core::Result<IAudioEndpointVolume> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
        unsafe { device.Activate(CLSCTX_ALL, None) }
    }

    fn default_audio_endpoint_is_muted() -> bool {
        default_audio_endpoint_volume()
            .and_then(|endpoint| unsafe { endpoint.GetMute() })
            .map(|muted| muted.as_bool())
            .unwrap_or(false)
    }

    fn process_running(process_id: u32) -> bool {
        if process_id == 0 {
            return false;
        }
        let Ok(process) = (unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                process_id,
            )
        }) else {
            return false;
        };
        let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
        unsafe { CloseHandle(process).ok() };
        running
    }

    fn spawn_launcher(root: &Path, python: &str, arguments: &[&str]) {
        let log_path = root.join(".run/panel-actions.log");
        let stdout = append_log(&log_path).ok();
        let stderr = append_log(&log_path).ok();
        let mut command = Command::new(python);
        command
            .arg(root.join("start_remote_desktop.py"))
            .args(arguments)
            .current_dir(root);
        if let Some(stdout) = stdout {
            command.stdout(Stdio::from(stdout));
        }
        if let Some(stderr) = stderr {
            command.stderr(Stdio::from(stderr));
        }
        use std::os::windows::process::CommandExt;
        command.creation_flags((DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).0);
        let _ = command.spawn();
    }

    fn append_log(path: &Path) -> std::io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }

    fn shell_open(target: &str, root: &Path) {
        let target = HSTRING::from(target);
        let directory = HSTRING::from(root.as_os_str().to_string_lossy().as_ref());
        let result =
            unsafe { ShellExecuteW(None, w!("open"), &target, None, &directory, SW_SHOWNORMAL) };
        if result.0 as isize <= 32 {
            unsafe {
                MessageBoxW(
                    None,
                    w!("无法打开目标。"),
                    PANEL_TITLE,
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }

    fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(value)?)
    }

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    }

    pub fn show_error(message: &str) {
        let message = HSTRING::from(message);
        unsafe {
            MessageBoxW(None, &message, PANEL_TITLE, MB_OK | MB_ICONERROR);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn privacy_stays_latched_after_disconnect_until_local_release() {
            let connected = next_privacy_latch(false, true, true, true);
            assert!(connected);
            assert!(next_privacy_latch(connected, false, true, true));
        }

        #[test]
        fn disabling_privacy_clears_a_disconnected_latch() {
            assert!(!next_privacy_latch(true, false, true, false));
        }

        #[test]
        fn injected_input_is_not_considered_physical() {
            let keyboard = KBDLLHOOKSTRUCT {
                flags: LLKHF_INJECTED,
                ..Default::default()
            };
            let mouse = MSLLHOOKSTRUCT {
                flags: LLMHF_INJECTED,
                ..Default::default()
            };
            assert!(!keyboard_input_is_physical(&keyboard));
            assert!(!mouse_input_is_physical(&mouse));
        }

        #[test]
        fn hardware_input_is_considered_physical() {
            assert!(keyboard_input_is_physical(&KBDLLHOOKSTRUCT::default()));
            assert!(mouse_input_is_physical(&MSLLHOOKSTRUCT::default()));
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_app::run() {
        windows_app::show_error(&error);
    }
}
