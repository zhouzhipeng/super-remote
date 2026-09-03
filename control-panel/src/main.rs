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
        net::{Ipv4Addr, SocketAddrV4, TcpListener},
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
                LRESULT, RECT, WAIT_TIMEOUT, WPARAM,
            },
            Graphics::Gdi::{
                BLACK_BRUSH, BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DEFAULT_CHARSET,
                DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
                EndPaint, FF_DONTCARE, FW_NORMAL, FillRect, GetStockObject, HBRUSH, HDC, HGDIOBJ,
                PAINTSTRUCT, PROOF_QUALITY, PS_SOLID, RoundRect, SelectObject, SetBkColor,
                SetBkMode, SetTextColor, TRANSPARENT,
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
                Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_SELECTED},
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                    SetProcessDpiAwarenessContext,
                },
                Shell::ShellExecuteW,
                WindowsAndMessaging::{
                    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_OWNERDRAW,
                    CREATESTRUCTW, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                    DispatchMessageW, ES_AUTOHSCROLL, ES_PASSWORD, FindWindowW, GWLP_USERDATA,
                    GetClientRect, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
                    GetWindowTextLengthW, GetWindowTextW, HHOOK, HMENU, HTTRANSPARENT,
                    HWND_TOPMOST, IDC_ARROW, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
                    LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED, LWA_ALPHA,
                    LoadCursorW, MB_ICONERROR, MB_OK, MSG, MSLLHOOKSTRUCT, MessageBoxW,
                    PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
                    SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE,
                    SW_RESTORE, SW_SHOWNA, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER,
                    SWP_SHOWWINDOW, SendMessageW, SetForegroundWindow, SetLayeredWindowAttributes,
                    SetTimer, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
                    SetWindowTextW, SetWindowsHookExW, ShowWindow, TranslateMessage,
                    UnhookWindowsHookEx, WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL, WH_MOUSE_LL,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE,
                    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM,
                    WM_GETFONT, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_SETFONT, WM_TIMER,
                    WNDCLASSW, WS_BORDER, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE,
                    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW,
                    WS_POPUP, WS_TABSTOP, WS_VISIBLE,
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
    const ID_SAVE_LOGIN: usize = 108;
    const ID_LOGIN_USERNAME: usize = 109;
    const ID_LOGIN_PASSWORD: usize = 110;
    const ID_WEB_PORT: usize = 111;
    const WM_LOCAL_PHYSICAL_INPUT: u32 = WM_APP + 1;

    const COLOR_BACKGROUND: COLORREF = rgb(244, 247, 251);
    const COLOR_HEADER: COLORREF = rgb(15, 23, 42);
    const COLOR_CARD: COLORREF = rgb(255, 255, 255);
    const COLOR_BORDER: COLORREF = rgb(226, 232, 240);
    const COLOR_TEXT: COLORREF = rgb(30, 41, 59);
    const COLOR_MUTED: COLORREF = rgb(100, 116, 139);
    const COLOR_PRIMARY: COLORREF = rgb(37, 99, 235);
    const COLOR_SUCCESS: COLORREF = rgb(22, 163, 74);
    const COLOR_DANGER: COLORREF = rgb(220, 38, 38);

    const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
        COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
    }

    static LOCAL_INPUT_WINDOW: AtomicIsize = AtomicIsize::new(0);
    static LOCAL_INPUT_ARMED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Default, Deserialize)]
    struct LauncherStatus {
        url: String,
        qr: String,
        #[serde(default = "default_web_port")]
        port: u16,
        host_pid: u32,
        signaling_pid: u32,
        primary_display: String,
        encoder: String,
        capture_mode: String,
        #[serde(default)]
        elevated: bool,
        #[serde(default)]
        launcher_executable: String,
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

    #[derive(Clone, Deserialize, Serialize)]
    struct StoredCredentials {
        jwt_secret: String,
        device_token: String,
        #[serde(default = "default_login_username")]
        username: String,
        password: String,
        #[serde(default = "default_web_port")]
        port: u16,
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
        title_label: HWND,
        subtitle_label: HWND,
        service_label: HWND,
        client_label: HWND,
        video_label: HWND,
        address_label: HWND,
        section_label: HWND,
        credential_help: HWND,
        policy_label: HWND,
        audio_policy_label: HWND,
        action_label: HWND,
        port_edit: HWND,
        username_edit: HWND,
        password_edit: HWND,
        start_button: HWND,
        stop_button: HWND,
        restart_button: HWND,
        open_web_button: HWND,
        open_qr_button: HWND,
        privacy_checkbox: HWND,
        host_mute_checkbox: HWND,
        overlay: HWND,
        privacy_supported: bool,
        privacy_visible: bool,
        privacy_latched: bool,
        client_connected: bool,
        service_buttons_running: Option<bool>,
        audio_mute: Option<AudioMuteLease>,
        settings: PanelSettings,
        background_brush: HBRUSH,
        header_brush: HBRUSH,
        card_brush: HBRUSH,
    }

    impl App {
        fn empty(root: PathBuf) -> Self {
            let run_dir = std::env::var_os("SUPER_REMOTE_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(".run"));
            Self {
                root,
                run_dir,
                window: HWND::default(),
                title_label: HWND::default(),
                subtitle_label: HWND::default(),
                service_label: HWND::default(),
                client_label: HWND::default(),
                video_label: HWND::default(),
                address_label: HWND::default(),
                section_label: HWND::default(),
                credential_help: HWND::default(),
                policy_label: HWND::default(),
                audio_policy_label: HWND::default(),
                action_label: HWND::default(),
                port_edit: HWND::default(),
                username_edit: HWND::default(),
                password_edit: HWND::default(),
                start_button: HWND::default(),
                stop_button: HWND::default(),
                restart_button: HWND::default(),
                open_web_button: HWND::default(),
                open_qr_button: HWND::default(),
                privacy_checkbox: HWND::default(),
                host_mute_checkbox: HWND::default(),
                overlay: HWND::default(),
                privacy_supported: false,
                privacy_visible: false,
                privacy_latched: false,
                client_connected: false,
                service_buttons_running: None,
                audio_mute: None,
                settings: PanelSettings::default(),
                background_brush: unsafe { CreateSolidBrush(COLOR_BACKGROUND) },
                header_brush: unsafe { CreateSolidBrush(COLOR_HEADER) },
                card_brush: unsafe { CreateSolidBrush(COLOR_CARD) },
            }
        }

        unsafe fn create_controls(&mut self, instance: HINSTANCE) -> windows::core::Result<()> {
            let dpi = unsafe { GetDpiForWindow(self.window) } as i32;
            let scale = |value: i32| value * dpi / 96;
            let base_font = unsafe {
                CreateFontW(
                    scale(-15),
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
                    w!("Segoe UI"),
                )
            };
            let title_font = unsafe {
                CreateFontW(
                    scale(-28),
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
                    w!("Segoe UI"),
                )
            };
            let section_font = unsafe {
                CreateFontW(
                    scale(-14),
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
                    w!("Segoe UI"),
                )
            };
            let detail_font = unsafe {
                CreateFontW(
                    scale(-13),
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
                    w!("Segoe UI"),
                )
            };

            self.title_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("Super Remote"),
                    scale(28),
                    scale(12),
                    scale(620),
                    scale(38),
                    0,
                    0,
                )?
            };
            self.subtitle_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("安全、轻量的局域网远程控制"),
                    scale(30),
                    scale(50),
                    scale(620),
                    scale(20),
                    0,
                    0,
                )?
            };
            unsafe {
                set_font(self.title_label, title_font.into());
                set_font(self.subtitle_label, detail_font.into());
            }

            self.service_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("服务状态：正在读取…"),
                    scale(42),
                    scale(103),
                    scale(616),
                    scale(24),
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
                    scale(42),
                    scale(135),
                    scale(616),
                    scale(24),
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
                    scale(42),
                    scale(167),
                    scale(616),
                    scale(24),
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
                    scale(42),
                    scale(199),
                    scale(616),
                    scale(24),
                    0,
                    0,
                )?
            };

            unsafe {
                self.start_button = child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("启动"),
                    scale(24),
                    scale(254),
                    scale(112),
                    scale(40),
                    BS_OWNERDRAW as u32,
                    ID_START,
                )?;
                self.stop_button = child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("停止"),
                    scale(24),
                    scale(254),
                    scale(112),
                    scale(40),
                    BS_OWNERDRAW as u32,
                    ID_STOP,
                )?;
                self.restart_button = child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("重启"),
                    scale(146),
                    scale(254),
                    scale(112),
                    scale(40),
                    BS_OWNERDRAW as u32,
                    ID_RESTART,
                )?;
                self.open_web_button = child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("打开网页"),
                    scale(268),
                    scale(254),
                    scale(134),
                    scale(40),
                    BS_OWNERDRAW as u32,
                    ID_OPEN_WEB,
                )?;
                self.open_qr_button = child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("查看二维码"),
                    scale(412),
                    scale(254),
                    scale(142),
                    scale(40),
                    BS_OWNERDRAW as u32,
                    ID_OPEN_QR,
                )?;
                for button in [
                    self.start_button,
                    self.stop_button,
                    self.restart_button,
                    self.open_web_button,
                    self.open_qr_button,
                ] {
                    set_font(button, section_font.into());
                }
            }

            self.section_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("连接与访问"),
                    scale(40),
                    scale(323),
                    scale(620),
                    scale(22),
                    0,
                    0,
                )?
            };
            let port_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("Web 端口"),
                    scale(40),
                    scale(353),
                    scale(90),
                    scale(20),
                    0,
                    0,
                )?
            };
            self.port_edit = unsafe {
                child(
                    self.window,
                    instance,
                    w!("EDIT"),
                    w!("8080"),
                    scale(40),
                    scale(375),
                    scale(90),
                    scale(34),
                    WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                    ID_WEB_PORT,
                )?
            };
            let username_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("登录账号"),
                    scale(148),
                    scale(353),
                    scale(100),
                    scale(20),
                    0,
                    0,
                )?
            };
            self.username_edit = unsafe {
                child(
                    self.window,
                    instance,
                    w!("EDIT"),
                    w!("admin"),
                    scale(148),
                    scale(375),
                    scale(190),
                    scale(34),
                    WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                    ID_LOGIN_USERNAME,
                )?
            };
            let password_label = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("登录密码"),
                    scale(356),
                    scale(353),
                    scale(100),
                    scale(20),
                    0,
                    0,
                )?
            };
            self.password_edit = unsafe {
                child(
                    self.window,
                    instance,
                    w!("EDIT"),
                    w!(""),
                    scale(356),
                    scale(375),
                    scale(172),
                    scale(34),
                    WS_BORDER.0 | ES_AUTOHSCROLL as u32 | ES_PASSWORD as u32,
                    ID_LOGIN_PASSWORD,
                )?
            };
            let save_login = unsafe {
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("保存并重启服务"),
                    scale(546),
                    scale(375),
                    scale(114),
                    scale(34),
                    BS_OWNERDRAW as u32,
                    ID_SAVE_LOGIN,
                )?
            };
            self.credential_help = unsafe {
                child(
                    self.window,
                    instance,
                    w!("STATIC"),
                    w!("端口范围 1–65535；密码至少 12 字节，留空则保留。"),
                    scale(40),
                    scale(419),
                    scale(620),
                    scale(20),
                    0,
                    0,
                )?
            };

            self.privacy_checkbox = unsafe {
                child(
                    self.window,
                    instance,
                    w!("BUTTON"),
                    w!("Web 客户端连接后启用本机隐私黑屏"),
                    scale(40),
                    scale(479),
                    scale(620),
                    scale(26),
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
                    scale(68),
                    scale(511),
                    scale(590),
                    scale(22),
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
                    scale(40),
                    scale(575),
                    scale(620),
                    scale(26),
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
                    scale(68),
                    scale(607),
                    scale(590),
                    scale(22),
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
                    scale(28),
                    scale(655),
                    scale(644),
                    scale(24),
                    0,
                    0,
                )?
            };

            for control in [
                self.service_label,
                self.client_label,
                self.video_label,
                self.address_label,
                self.section_label,
                port_label,
                self.port_edit,
                username_label,
                self.username_edit,
                password_label,
                self.password_edit,
                self.credential_help,
                save_login,
                self.policy_label,
                self.audio_policy_label,
                self.action_label,
                self.privacy_checkbox,
                self.host_mute_checkbox,
            ] {
                unsafe { set_font(control, base_font.into()) };
            }
            unsafe {
                set_font(self.section_label, section_font.into());
                set_font(save_login, section_font.into());
                set_font(self.credential_help, detail_font.into());
                set_font(self.policy_label, detail_font.into());
                set_font(self.audio_policy_label, detail_font.into());
                set_font(self.action_label, detail_font.into());
            }

            self.settings =
                read_json(&self.run_dir.join("control-settings.json")).unwrap_or_default();
            let (username, port) =
                read_json::<StoredCredentials>(&self.run_dir.join("secrets.json"))
                    .map(|credentials| (credentials.username, credentials.port))
                    .unwrap_or_else(|| (default_login_username(), default_web_port()));
            unsafe {
                set_text(self.username_edit, &username);
                set_text(self.port_edit, &port.to_string());
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
            self.sync_service_buttons(services_running);

            set_text_if_changed(
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
            set_text_if_changed(
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
            set_text_if_changed(
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
            set_text_if_changed(
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

        fn sync_service_buttons(&mut self, services_running: bool) {
            if self.service_buttons_running == Some(services_running) {
                return;
            }
            unsafe {
                let _ = ShowWindow(
                    self.start_button,
                    if services_running { SW_HIDE } else { SW_SHOWNA },
                );
                for button in [
                    self.stop_button,
                    self.restart_button,
                    self.open_web_button,
                    self.open_qr_button,
                ] {
                    let _ = ShowWindow(button, if services_running { SW_SHOWNA } else { SW_HIDE });
                }
            }
            self.service_buttons_running = Some(services_running);
        }

        fn set_privacy_visible(&mut self, visible: bool) {
            if self.overlay.0.is_null() {
                return;
            }
            let visibility_changed = self.privacy_visible != visible;
            if visibility_changed {
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
            if visible {
                set_text_if_changed(
                    self.action_label,
                    if self.client_connected {
                        "所有显示器隐私黑屏已启用；断线后需使用本机键盘或鼠标解除。"
                    } else {
                        "客户端已断开；正在等待本机键盘或鼠标操作解除隐私黑屏。"
                    },
                );
            } else if visibility_changed {
                set_text_if_changed(self.action_label, "");
            }
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

        fn save_login_credentials(&mut self) {
            let port = match parse_web_port(&window_text(self.port_edit)) {
                Ok(port) => port,
                Err(error) => {
                    set_text(self.action_label, &error);
                    return;
                }
            };
            let username = window_text(self.username_edit).trim().to_owned();
            let new_password = window_text(self.password_edit);
            if let Err(error) = validate_login_update(
                &username,
                (!new_password.is_empty()).then_some(new_password.as_str()),
            ) {
                set_text(self.action_label, &error);
                return;
            }
            let current_port = read_json::<LauncherStatus>(&self.run_dir.join("status.json"))
                .map(|status| status.port)
                .unwrap_or_else(default_web_port);
            if port != current_port && !web_port_is_available(port) {
                set_text(
                    self.action_label,
                    &format!("端口 {port} 已被其他程序占用；设置未保存。"),
                );
                return;
            }
            let path = self.run_dir.join("secrets.json");
            let Some(mut credentials) = read_json::<StoredCredentials>(&path) else {
                set_text(
                    self.action_label,
                    "无法读取 .run/secrets.json；凭据未修改。",
                );
                return;
            };
            credentials.username = username;
            credentials.port = port;
            if !new_password.is_empty() {
                credentials.password = new_password;
            }
            if let Err(error) = write_json(&path, &credentials) {
                set_text(self.action_label, &format!("无法保存登录凭据：{error}"));
                return;
            }
            set_text(self.password_edit, "");
            self.action(Action::Restart);
            set_text(
                self.action_label,
                &format!("Web 设置已保存；正在重启端口 {port} 的服务…"),
            );
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
                    let executable = if launcher.launcher_executable.is_empty() {
                        self.root.join("super-remote.exe")
                    } else {
                        PathBuf::from(launcher.launcher_executable)
                    };
                    let mut arguments = vec!["--from-control-panel"];
                    if matches!(kind, Action::Stop) {
                        arguments.push("--stop");
                    }
                    spawn_launcher(&self.root, &executable, &arguments);
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
                    let _ = ShowWindow(existing, SW_RESTORE);
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
        let panel_width = 700;
        let panel_height = 720;
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
            let dpi = GetDpiForWindow(app.window) as i32;
            let scale = |value: i32| value * dpi / 96;
            let scaled_width = scale(700);
            let scaled_height = scale(720);
            let scaled_x = (GetSystemMetrics(SM_CXSCREEN) - scaled_width) / 2;
            let scaled_y = (GetSystemMetrics(SM_CYSCREEN) - scaled_height) / 2;
            SetWindowPos(
                app.window,
                None,
                scaled_x,
                scaled_y,
                scaled_width,
                scaled_height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .map_err(|error| error.to_string())?;
            app.create_controls(instance)
                .map_err(|error| error.to_string())?;
            app.create_overlay(instance)
                .map_err(|error| error.to_string())?;
            let _local_input_hooks = LocalInputHooks::install(app.window, instance)?;
            app.refresh();
            let _ = ShowWindow(app.window, SW_SHOWNORMAL);
            SetTimer(Some(app.window), 1, 500, None);

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
        let executable = std::env::current_exe().ok();
        if let Some(directory) = executable.as_deref().and_then(Path::parent)
            && directory.join("super-remote.exe").is_file()
        {
            return directory.to_path_buf();
        }
        executable
            .and_then(|path| path.parent()?.parent()?.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn default_login_username() -> String {
        "admin".into()
    }

    fn default_web_port() -> u16 {
        8080
    }

    fn parse_web_port(value: &str) -> Result<u16, String> {
        match value.trim().parse::<u16>() {
            Ok(port) if port != 0 => Ok(port),
            _ => Err("Web 端口必须是 1 到 65535 之间的整数。".into()),
        }
    }

    fn web_port_is_available(port: u16) -> bool {
        TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).is_ok()
    }

    fn validate_login_update(username: &str, password: Option<&str>) -> Result<(), String> {
        if username.is_empty() {
            return Err("登录账号不能为空。".into());
        }
        if username.len() > 128 || username.chars().any(char::is_control) {
            return Err("登录账号不能包含控制字符，且最多为 128 个 UTF-8 字节。".into());
        }
        if let Some(password) = password {
            if password.len() < 12 {
                return Err("登录密码至少需要 12 个 UTF-8 字节。".into());
            }
            if password.len() > 256 || password.chars().any(char::is_control) {
                return Err("登录密码不能包含控制字符，且最多为 256 个 UTF-8 字节。".into());
            }
        }
        Ok(())
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
        let panel_brush = unsafe { CreateSolidBrush(COLOR_BACKGROUND) };
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

    unsafe fn paint_panel(window: HWND, app: &App) {
        let mut paint = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(window, &mut paint) };
        let mut client = RECT::default();
        if unsafe { GetClientRect(window, &mut client) }.is_ok() {
            unsafe {
                FillRect(hdc, &client, app.background_brush);
            }
            let dpi = unsafe { GetDpiForWindow(window) } as i32;
            let scale = |value: i32| value * dpi / 96;
            let header = RECT {
                left: 0,
                top: 0,
                right: client.right,
                bottom: scale(78),
            };
            unsafe {
                FillRect(hdc, &header, app.header_brush);
            }

            let border_pen = unsafe { CreatePen(PS_SOLID, 1, COLOR_BORDER) };
            let old_pen = unsafe { SelectObject(hdc, border_pen.into()) };
            let old_brush = unsafe { SelectObject(hdc, app.card_brush.into()) };
            for (left, top, right, bottom) in [
                (24, 92, 676, 238),
                (24, 310, 676, 451),
                (24, 467, 676, 547),
                (24, 563, 676, 643),
            ] {
                unsafe {
                    let _ = RoundRect(
                        hdc,
                        scale(left),
                        scale(top),
                        scale(right),
                        scale(bottom),
                        scale(12),
                        scale(12),
                    );
                }
            }
            unsafe {
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
                let _ = DeleteObject(border_pen.into());
            }
        }
        unsafe {
            let _ = EndPaint(window, &paint);
        }
    }

    unsafe fn paint_static(app: &App, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let hdc = HDC(wparam.0 as *mut c_void);
        let control = HWND(lparam.0 as *mut c_void);
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
        }
        let (text, brush) = if control == app.title_label {
            (rgb(248, 250, 252), app.header_brush)
        } else if control == app.subtitle_label {
            (rgb(203, 213, 225), app.header_brush)
        } else if control == app.credential_help
            || control == app.policy_label
            || control == app.audio_policy_label
        {
            (COLOR_MUTED, app.card_brush)
        } else if control == app.action_label {
            (COLOR_PRIMARY, app.background_brush)
        } else {
            (COLOR_TEXT, app.card_brush)
        };
        unsafe {
            SetTextColor(hdc, text);
        }
        LRESULT(brush.0 as isize)
    }

    unsafe fn paint_button_background(app: &App, wparam: WPARAM) -> LRESULT {
        let hdc = HDC(wparam.0 as *mut c_void);
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLOR_TEXT);
        }
        LRESULT(app.card_brush.0 as isize)
    }

    unsafe fn paint_edit_background(app: &App, wparam: WPARAM) -> LRESULT {
        let hdc = HDC(wparam.0 as *mut c_void);
        unsafe {
            SetBkColor(hdc, COLOR_CARD);
            SetTextColor(hdc, COLOR_TEXT);
        }
        LRESULT(app.card_brush.0 as isize)
    }

    unsafe fn draw_owner_button(lparam: LPARAM) {
        let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let (mut background, mut border, mut foreground) = match item.CtlID as usize {
            ID_START => (COLOR_SUCCESS, COLOR_SUCCESS, rgb(255, 255, 255)),
            ID_STOP => (rgb(254, 242, 242), rgb(254, 202, 202), COLOR_DANGER),
            ID_RESTART => (COLOR_CARD, COLOR_BORDER, COLOR_TEXT),
            ID_OPEN_WEB => (COLOR_PRIMARY, COLOR_PRIMARY, rgb(255, 255, 255)),
            ID_OPEN_QR => (rgb(239, 246, 255), rgb(191, 219, 254), COLOR_PRIMARY),
            ID_SAVE_LOGIN => (COLOR_HEADER, COLOR_HEADER, rgb(255, 255, 255)),
            _ => (COLOR_CARD, COLOR_BORDER, COLOR_TEXT),
        };
        if disabled {
            background = rgb(241, 245, 249);
            border = COLOR_BORDER;
            foreground = rgb(148, 163, 184);
        } else if pressed {
            background = match item.CtlID as usize {
                ID_START => rgb(21, 128, 61),
                ID_STOP => rgb(254, 226, 226),
                ID_RESTART => rgb(241, 245, 249),
                ID_OPEN_WEB => rgb(29, 78, 216),
                ID_OPEN_QR => rgb(219, 234, 254),
                ID_SAVE_LOGIN => rgb(30, 41, 59),
                _ => background,
            };
        }

        let brush = unsafe { CreateSolidBrush(background) };
        let pen = unsafe { CreatePen(PS_SOLID, 1, border) };
        let old_brush = unsafe { SelectObject(item.hDC, brush.into()) };
        let old_pen = unsafe { SelectObject(item.hDC, pen.into()) };
        unsafe {
            let _ = RoundRect(
                item.hDC,
                item.rcItem.left,
                item.rcItem.top,
                item.rcItem.right,
                item.rcItem.bottom,
                10,
                10,
            );
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(item.hDC, foreground);
        }
        let font = unsafe { SendMessageW(item.hwndItem, WM_GETFONT, None, None) };
        let old_font = if font.0 != 0 {
            Some(unsafe { SelectObject(item.hDC, HGDIOBJ(font.0 as *mut c_void)) })
        } else {
            None
        };
        let mut text = window_text(item.hwndItem)
            .encode_utf16()
            .collect::<Vec<_>>();
        let mut text_rect = item.rcItem;
        unsafe {
            DrawTextW(
                item.hDC,
                &mut text,
                &mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        if let Some(old_font) = old_font {
            unsafe {
                SelectObject(item.hDC, old_font);
            }
        }
        unsafe {
            SelectObject(item.hDC, old_pen);
            SelectObject(item.hDC, old_brush);
            let _ = DeleteObject(pen.into());
            let _ = DeleteObject(brush.into());
        }
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
            WM_PAINT if !app_ptr.is_null() => {
                unsafe { paint_panel(window, &*app_ptr) };
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC if !app_ptr.is_null() => unsafe {
                paint_static(&*app_ptr, wparam, lparam)
            },
            WM_CTLCOLORBTN if !app_ptr.is_null() => unsafe {
                paint_button_background(&*app_ptr, wparam)
            },
            WM_CTLCOLOREDIT if !app_ptr.is_null() => unsafe {
                paint_edit_background(&*app_ptr, wparam)
            },
            WM_DRAWITEM if !app_ptr.is_null() => {
                unsafe { draw_owner_button(lparam) };
                LRESULT(1)
            }
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
                        ID_SAVE_LOGIN => app.save_login_credentials(),
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

    /// Update one native control only when its visible text actually changed.
    /// The status timer runs twice per second; avoiding redundant SetWindowTextW
    /// calls prevents transparent STATIC controls from erasing and repainting
    /// their card background on every tick.
    fn set_text_if_changed(window: HWND, text: &str) -> bool {
        if window.0.is_null() || window_text(window) == text {
            return false;
        }
        set_text(window, text);
        true
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

    fn spawn_launcher(root: &Path, executable: &Path, arguments: &[&str]) {
        let data_dir = std::env::var_os("SUPER_REMOTE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".run"));
        let log_path = data_dir.join("panel-actions.log");
        let stdout = append_log(&log_path).ok();
        let stderr = append_log(&log_path).ok();
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .current_dir(root)
            .env("SUPER_REMOTE_DATA_DIR", &data_dir);
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

    fn window_text(window: HWND) -> String {
        let length = unsafe { GetWindowTextLengthW(window) };
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0_u16; length as usize + 1];
        let copied = unsafe { GetWindowTextW(window, &mut buffer) };
        String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
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

        #[test]
        fn legacy_credentials_default_to_admin() {
            let credentials: StoredCredentials = serde_json::from_str(
                r#"{"jwt_secret":"secret","device_token":"device","password":"long-password"}"#,
            )
            .unwrap();
            assert_eq!(credentials.username, "admin");
            assert_eq!(credentials.port, 8080);
        }

        #[test]
        fn login_update_requires_a_nonempty_user_and_long_password() {
            assert!(validate_login_update("operator", None).is_ok());
            assert!(validate_login_update("", None).is_err());
            assert!(validate_login_update("operator", Some("too-short")).is_err());
            assert!(validate_login_update("operator", Some("long-password")).is_ok());
        }

        #[test]
        fn web_port_must_fit_the_tcp_port_range() {
            assert_eq!(parse_web_port(" 8080 ").unwrap(), 8080);
            assert!(parse_web_port("0").is_err());
            assert!(parse_web_port("65536").is_err());
            assert!(parse_web_port("not-a-port").is_err());
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_app::run() {
        windows_app::show_error(&error);
    }
}
