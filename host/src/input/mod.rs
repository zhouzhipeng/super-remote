#[cfg(windows)]
mod windows_input;

use remote_protocol::input::TimedInputEvent;

pub fn inject_packet(packet: &[u8]) -> anyhow::Result<()> {
    let event = TimedInputEvent::decode(packet)?;
    #[cfg(windows)]
    return windows_input::inject(event.event);
    #[cfg(not(windows))]
    anyhow::bail!("input injection is only supported on Windows")
}
