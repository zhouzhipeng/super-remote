#[cfg(windows)]
mod windows_input;

use remote_protocol::input::TimedInputEvent;

/// Shared by every transport in one session. The mutex must cover injection as
/// well as the watermark: independent channels may run on different threads.
#[derive(Default)]
pub struct SessionInput {
    latest_position: u64,
    latest_input: u64,
    input_at: Option<std::time::Instant>,
    wheel_at: Option<std::time::Instant>,
    held: PressedInputs,
    using_control: bool,
}

impl SessionInput {
    pub fn refinement_ready(&self) -> bool {
        self.activity().1 >= std::time::Duration::from_millis(250)
            && self.wheel_at.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(900))
    }

    pub fn activity(&self) -> (u64, std::time::Duration) {
        (
            self.latest_input,
            self.input_at
                .map_or(std::time::Duration::MAX, |at| at.elapsed()),
        )
    }

    fn is_stale_move(&self, event: &TimedInputEvent) -> bool {
        matches!(
            event.event,
            remote_protocol::input::InputEvent::MouseMove { .. }
        ) && event.timestamp_us != 0
            && event.timestamp_us <= self.latest_position
    }

    fn observe(&mut self, event: TimedInputEvent) {
        use remote_protocol::input::InputEvent;
        if matches!(
            event.event,
            InputEvent::MouseMove { .. }
                | InputEvent::MouseButton {
                    position: Some(_),
                    ..
                }
        ) {
            self.latest_position = self.latest_position.max(event.timestamp_us);
        }
        // Typing and pointer motion retain sharp incremental updates.
        // Only mouse buttons/wheel suspend them for the interactive video path.
        if !matches!(event.event, InputEvent::MouseMove { .. } | InputEvent::MouseRelative { .. } | InputEvent::Keyboard { .. }) {
            self.latest_input = self.latest_input.max(event.timestamp_us);
            self.input_at = Some(std::time::Instant::now());
        }
        if matches!(event.event, InputEvent::MouseWheel { .. }) {
            self.wheel_at = Some(std::time::Instant::now());
        }
        self.held.observe(event.event);
    }

    pub fn inject(&mut self, packet: &[u8]) -> anyhow::Result<Option<TimedInputEvent>> {
        if self.using_control {
            return Ok(None);
        }
        self.inject_ordered(packet)
    }

    pub fn inject_control(&mut self, packet: &[u8]) -> anyhow::Result<Option<TimedInputEvent>> {
        // Decode before committing the permanent fallback. Late RTC messages
        // and its eventual close must not affect keys held on the new path.
        TimedInputEvent::decode(packet)?;
        if !self.using_control {
            self.release_all();
            self.using_control = true;
        }
        self.inject_ordered(packet)
    }

    pub fn release_rtc(&mut self) {
        if !self.using_control {
            self.release_all();
        }
    }

    fn inject_ordered(&mut self, packet: &[u8]) -> anyhow::Result<Option<TimedInputEvent>> {
        let event = TimedInputEvent::decode(packet)?;
        if self.is_stale_move(&event) {
            return Ok(None);
        }
        let event = inject_packet(packet)?;
        self.observe(event);
        Ok(Some(event))
    }

    pub fn release_all(&mut self) {
        for packet in self.held.release_packets() {
            let _ = inject_packet(&packet);
        }
    }
}

#[derive(Default)]
pub struct PressedInputs {
    keys: std::collections::HashSet<(u16, bool)>,
    buttons: std::collections::HashSet<u8>,
}
impl PressedInputs {
    pub fn observe(&mut self, event: remote_protocol::input::InputEvent) {
        use remote_protocol::input::InputEvent;
        match event {
            InputEvent::Keyboard {
                scan_code,
                down,
                extended,
            } => {
                if down {
                    self.keys.insert((scan_code, extended));
                } else {
                    self.keys.remove(&(scan_code, extended));
                }
            }
            InputEvent::MouseButton { button, down, .. } => {
                if down {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
            }
            _ => {}
        }
    }
    pub fn release_packets(&mut self) -> Vec<Vec<u8>> {
        use remote_protocol::input::InputEvent;
        self.keys
            .drain()
            .map(|(scan_code, extended)| InputEvent::Keyboard {
                scan_code,
                extended,
                down: false,
            })
            .chain(self.buttons.drain().map(|button| InputEvent::MouseButton {
                button,
                down: false,
                position: None,
            }))
            .map(|event| {
                TimedInputEvent {
                    flags: 0,
                    timestamp_us: 0,
                    event,
                }
                .encode()
            })
            .collect()
    }
}

/// A private reactor keeps SendInput and its ACK off video/signaling executors.
/// HIGHEST is used within the normal process class, never REALTIME/TIME_CRITICAL.
pub fn spawn_priority(
    name: String,
    work: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new().name(name).spawn(move || {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
            };
            if let Err(error) = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) {
                tracing::warn!(%error, "input worker could not raise thread priority");
            }
        }
        runtime.block_on(work);
    })?;
    Ok(())
}

#[cfg(test)]
mod worker_tests {
    #[test]
    fn scroll_gaps_keep_video_running_without_delaying_typing() {
        let mut state = super::SessionInput::default();
        assert!(state.refinement_ready());
        state.input_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(500));
        state.wheel_at = state.input_at;
        assert!(!state.refinement_ready());
        state.wheel_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(950));
        assert!(state.refinement_ready());
        state.input_at = Some(std::time::Instant::now());
        assert!(!state.refinement_ready());
    }

    #[test]
    fn typing_does_not_suspend_or_cancel_sharp_updates() {
        let mut state = super::SessionInput::default();
        for down in [true, false] {
            state.observe(remote_protocol::input::TimedInputEvent {
                timestamp_us: 100, flags: 0,
                event: remote_protocol::input::InputEvent::Keyboard { scan_code: 30, down, extended: false },
            });
            assert_eq!(state.activity(), (0, std::time::Duration::MAX));
        }
    }

    #[test]
    fn late_rtc_packets_and_close_do_not_release_fallback_keys() {
        use remote_protocol::input::{InputEvent, TimedInputEvent};
        let mut state = super::SessionInput::default();
        state.using_control = true;
        state.held.observe(InputEvent::Keyboard {
            scan_code: 30,
            down: true,
            extended: false,
        });
        let packet = TimedInputEvent {
            timestamp_us: 1,
            flags: 0,
            event: InputEvent::Keyboard {
                scan_code: 30,
                down: false,
                extended: false,
            },
        }
        .encode();
        assert!(state.inject(&packet).unwrap().is_none());
        state.release_rtc();
        assert_eq!(state.held.release_packets().len(), 1);
    }
    #[test]
    fn delayed_moves_cannot_undo_clicks_but_reliable_transitions_survive() {
        use remote_protocol::input::{InputEvent, TimedInputEvent};
        let mut state = super::SessionInput::default();
        let move_at = |timestamp_us| TimedInputEvent {
            timestamp_us,
            flags: 0,
            event: InputEvent::MouseMove { x: 1, y: 2 },
        };
        state.observe(move_at(10));
        assert_eq!(state.activity(), (0, std::time::Duration::MAX));
        assert!(state.is_stale_move(&move_at(9)));
        assert!(state.is_stale_move(&move_at(10)));
        let click = TimedInputEvent {
            timestamp_us: 20,
            flags: 0,
            event: InputEvent::MouseButton {
                button: 0,
                down: true,
                position: Some((50, 60)),
            },
        };
        state.observe(click);
        assert!(state.is_stale_move(&move_at(19)));
        assert!(!state.is_stale_move(&move_at(21)));
        assert!(!state.is_stale_move(&click));
        let input_at = state.input_at;
        state.observe(move_at(30));
        assert_eq!(state.latest_input, 20);
        assert_eq!(state.input_at, input_at);
        state.observe(click);
        assert!(state.is_stale_move(&move_at(29)));
    }
    #[test]
    fn disconnect_releases_held_keys_and_buttons_without_replaying_downs() {
        use remote_protocol::input::InputEvent;
        let mut held = super::PressedInputs::default();
        held.observe(InputEvent::Keyboard {
            scan_code: 30,
            extended: false,
            down: true,
        });
        held.observe(InputEvent::MouseButton {
            button: 0,
            position: None,
            down: true,
        });
        let packets = held.release_packets();
        assert_eq!(packets.len(), 2);
        for data in packets {
            let event = super::TimedInputEvent::decode(&data).unwrap();
            assert!(matches!(
                event.event,
                InputEvent::Keyboard { down: false, .. }
                    | InputEvent::MouseButton { down: false, .. }
            ));
        }
        assert!(held.release_packets().is_empty());
    }
    #[tokio::test]
    async fn input_runs_on_an_independent_high_priority_thread() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        super::spawn_priority("test-input-priority".into(), async move {
            #[cfg(windows)]
            unsafe {
                use windows::Win32::System::Threading::{
                    GetCurrentThread, GetThreadPriority, THREAD_PRIORITY_HIGHEST,
                };
                assert_eq!(
                    GetThreadPriority(GetCurrentThread()),
                    THREAD_PRIORITY_HIGHEST.0
                );
            }
            tx.send(std::thread::current().name().unwrap().to_owned())
                .unwrap();
        })
        .unwrap();
        assert_eq!(rx.await.unwrap(), "test-input-priority");
    }
}

pub fn inject_packet(packet: &[u8]) -> anyhow::Result<TimedInputEvent> {
    let event = TimedInputEvent::decode(packet)?;
    #[cfg(windows)]
    {
        windows_input::inject(event.event)?;
        Ok(event)
    }
    #[cfg(not(windows))]
    anyhow::bail!("input injection is only supported on Windows")
}

pub fn paste_text(text: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows_input::paste_text(text)
    }
    #[cfg(not(windows))]
    anyhow::bail!("input injection is only supported on Windows")
}

pub fn paste_clipboard() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows_input::paste_clipboard()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows required")
    }
}
