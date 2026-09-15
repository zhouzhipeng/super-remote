#[cfg(windows)]
mod windows_input;

use remote_protocol::input::TimedInputEvent;

/// How long a mouse button suspends *presentation* of the sharp layer. It covers
/// the asynchronous repaint that follows a click, not the refinement pipeline:
/// capture and delta commits continue throughout, so the sharp layer is already
/// current when the window closes.
const BUTTON_PRESENTATION_WINDOW: std::time::Duration = std::time::Duration::from_millis(200);
/// The wheel equivalent. This was 900 ms, chosen so a scroll could never re-show
/// a pre-scroll baseline. Correctness now comes from evidence rather than from
/// waiting: the refinement worker must observe the scene holding still against
/// the committed baseline before that baseline may certify the input state.
const WHEEL_PRESENTATION_WINDOW: std::time::Duration = std::time::Duration::from_millis(220);

/// How often a queued wheel slice is injected. 120 Hz is above any display's
/// scroll cadence, so pacing is never what the eye sees.
pub const WHEEL_TICK: std::time::Duration = std::time::Duration::from_millis(8);

/// Spreads a wheel burst over time instead of injecting it as one jump.
///
/// A browser coalesces wheel events per frame, so a fast flick reaches the Host
/// as a single packet carrying several notches at once; Windows applies all of
/// it in one step. Splitting that back into steps of at most `step` units - and
/// never finer than the client itself sent, so precision-touchpad input that is
/// already pixel-accurate passes straight through - is what makes a flick read
/// as motion. Net displacement is preserved exactly: nothing is invented and
/// nothing is dropped.
#[derive(Debug)]
pub struct WheelSmoother {
    step: i32,
    pending_x: i32,
    pending_y: i32,
    slice_x: i32,
    slice_y: i32,
    injected_at: Option<std::time::Instant>,
}

impl Default for WheelSmoother {
    fn default() -> Self {
        Self::new(120)
    }
}

impl WheelSmoother {
    pub fn new(step: u16) -> Self {
        Self {
            step: i32::from(step).max(1),
            pending_x: 0,
            pending_y: 0,
            slice_x: 0,
            slice_y: 0,
            injected_at: None,
        }
    }

    /// Whether a slice may be injected now.
    ///
    /// A relay delivers wheel packets in clumps after a stall, and injecting a
    /// clump microseconds apart is one jump to the remote application - which
    /// Chromium is also documented to drop outright when fine wheel events
    /// arrive in a burst. Spacing engages only above the tick rate, so ordinary
    /// per-frame scrolling is never held back and a scroll still starts on the
    /// packet that begins it.
    fn ready(&self, now: std::time::Instant) -> bool {
        self.injected_at
            .is_none_or(|at| now.saturating_duration_since(at) >= WHEEL_TICK)
    }

    fn push(&mut self, delta_x: i16, delta_y: i16) {
        self.pending_x = self.pending_x.saturating_add(i32::from(delta_x));
        self.pending_y = self.pending_y.saturating_add(i32::from(delta_y));
        if delta_x != 0 {
            self.slice_x = self.step.min(i32::from(delta_x).abs());
        }
        if delta_y != 0 {
            self.slice_y = self.step.min(i32::from(delta_y).abs());
        }
    }

    /// The next delta to inject, or `None` when the burst has drained or the
    /// previous slice is still too recent.
    fn take(&mut self, now: std::time::Instant) -> Option<(i16, i16)> {
        if !self.ready(now) {
            return None;
        }
        let x = take_axis(&mut self.pending_x, self.slice_x);
        let y = take_axis(&mut self.pending_y, self.slice_y);
        let slice = (x != 0 || y != 0).then_some((x, y));
        if slice.is_some() {
            self.injected_at = Some(now);
        }
        slice
    }

    fn pending(&self) -> bool {
        self.pending_x != 0 || self.pending_y != 0
    }
}

fn take_axis(pending: &mut i32, slice: i32) -> i16 {
    if *pending == 0 || slice <= 0 {
        return 0;
    }
    let delta = pending.abs().min(slice) * pending.signum();
    *pending -= delta;
    // `slice` is bounded by the configured step, which `HostConfig` keeps inside
    // i16; the clamp only makes that dependency impossible to break silently.
    delta.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

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
    wheel: WheelSmoother,
    /// Woken when a burst leaves a remainder, so the session's paced injector
    /// does not have to poll an idle pointer.
    wheel_wake: std::sync::Arc<tokio::sync::Notify>,
}

impl SessionInput {
    pub fn with_wheel_step(step: u16) -> Self {
        Self {
            wheel: WheelSmoother::new(step),
            ..Default::default()
        }
    }

    /// Signalled when a wheel burst still has slices to inject.
    pub fn wheel_wake(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.wheel_wake.clone()
    }

    pub fn wheel_pending(&self) -> bool {
        self.wheel.pending()
    }

    /// Inject the next queued wheel slice. A no-op once the burst has drained,
    /// so a spurious wake costs one lock and nothing else.
    pub fn drain_wheel(&mut self) {
        if let Some((delta_x, delta_y)) = self.wheel.take(std::time::Instant::now())
            && let Err(error) = inject_event(remote_protocol::input::InputEvent::MouseWheel {
                delta_x,
                delta_y,
            })
        {
            tracing::warn!(%error, "paced wheel slice was rejected");
            self.wheel = WheelSmoother::new(self.wheel.step.max(1) as u16);
        }
    }

    /// Whether the lossless layer may be shown as a picture of *now*. It does
    /// not gate capture or transmission; see `desktop_tiles::serve`.
    pub fn presentation_ready(&self) -> bool {
        self.activity().1 >= BUTTON_PRESENTATION_WINDOW
            && self
                .wheel_at
                .is_none_or(|at| at.elapsed() >= WHEEL_PRESENTATION_WINDOW)
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
        use remote_protocol::input::InputEvent;
        let event = TimedInputEvent::decode(packet)?;
        if self.is_stale_move(&event) {
            return Ok(None);
        }
        if let InputEvent::MouseWheel { delta_x, delta_y } = event.event {
            // The first slice goes out on this thread, so a scroll still starts
            // with no added latency; only the tail of a burst is paced.
            self.wheel.push(delta_x, delta_y);
            self.drain_wheel();
            if self.wheel.pending() {
                self.wheel_wake.notify_one();
            }
        } else {
            inject_event(event.event)?;
        }
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
        let ago = |ms| Some(std::time::Instant::now() - std::time::Duration::from_millis(ms));
        let mut state = super::SessionInput::default();
        assert!(state.presentation_ready());
        // Inside the wheel window the sharp layer stays suppressed even though
        // the shared button window has already elapsed.
        state.input_at = ago(210);
        state.wheel_at = state.input_at;
        assert!(!state.presentation_ready());
        state.wheel_at = ago(230);
        assert!(state.presentation_ready());
        // Any fresh button/wheel packet reopens the window immediately.
        state.input_at = Some(std::time::Instant::now());
        assert!(!state.presentation_ready());
    }

    #[test]
    fn wheel_bursts_are_paced_without_inventing_or_losing_motion() {
        // One shared clock that only ever advances, a full tick per attempt:
        // pacing is asserted separately, this walks each burst to the end.
        let clock = std::cell::Cell::new(std::time::Instant::now());
        let drain = |smoother: &mut super::WheelSmoother| {
            let mut slices = Vec::new();
            loop {
                clock.set(clock.get() + super::WHEEL_TICK);
                let Some(slice) = smoother.take(clock.get()) else { break };
                slices.push(slice);
                assert!(slices.len() < 64, "a burst must always drain");
            }
            slices
        };
        // One notch is injected whole: applications that only understand whole
        // notches keep behaving exactly as they did.
        let mut wheel = super::WheelSmoother::new(120);
        wheel.push(0, 120);
        assert_eq!(drain(&mut wheel), vec![(0, 120)]);
        assert!(!wheel.pending());

        // A flick coalesced into one five-notch packet becomes five steps.
        wheel.push(0, -600);
        assert_eq!(drain(&mut wheel), vec![(0, -120); 5]);

        // Precision input is never subdivided below what the client sent.
        wheel.push(0, 6);
        assert_eq!(drain(&mut wheel), vec![(0, 6)]);

        // A smaller step glides through a single notch instead of stepping it.
        let mut fine = super::WheelSmoother::new(40);
        fine.push(0, 120);
        assert_eq!(drain(&mut fine), vec![(0, 40); 3]);
        fine.push(0, 6);
        assert_eq!(drain(&mut fine), vec![(0, 6)]);

        // Net displacement is exact, including a reversal mid-burst and both
        // axes draining together.
        let mut mixed = super::WheelSmoother::new(120);
        mixed.push(240, -360);
        mixed.push(-120, 120);
        let total = drain(&mut mixed)
            .iter()
            .fold((0i32, 0i32), |sum, slice| {
                (sum.0 + i32::from(slice.0), sum.1 + i32::from(slice.1))
            });
        assert_eq!(total, (120, -240));
        assert!(!mixed.pending());

        // An extreme delta cannot produce a slice outside the protocol's i16.
        let mut wide = super::WheelSmoother::new(32767);
        wide.push(i16::MIN, i16::MAX);
        for (x, y) in drain(&mut wide) {
            assert!(i32::from(x).abs() <= 32767 && i32::from(y).abs() <= 32767);
        }
    }

    #[test]
    fn a_clump_of_wheel_packets_is_spaced_instead_of_injected_at_once() {
        let start = std::time::Instant::now();
        let mut wheel = super::WheelSmoother::new(120);
        // The packet that begins a scroll is never held back.
        wheel.push(0, 120);
        assert_eq!(wheel.take(start), Some((0, 120)));
        // A second packet arriving in the same clump waits for the tick rather
        // than landing on top of the first as one jump.
        wheel.push(0, 120);
        assert_eq!(wheel.take(start + std::time::Duration::from_millis(1)), None);
        assert!(wheel.pending(), "a held slice is still owed");
        assert_eq!(wheel.take(start + super::WHEEL_TICK), Some((0, 120)));
        // Ordinary per-frame scrolling is slower than the tick, so it is never
        // delayed: the third notch injects on arrival.
        let frame = start + std::time::Duration::from_millis(16);
        wheel.push(0, 120);
        assert_eq!(wheel.take(frame), Some((0, 120)));
        assert!(!wheel.pending());
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
    inject_event(event.event)?;
    Ok(event)
}

pub fn inject_event(event: remote_protocol::input::InputEvent) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows_input::inject(event)
    }
    #[cfg(not(windows))]
    {
        let _ = event;
        anyhow::bail!("input injection is only supported on Windows")
    }
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
