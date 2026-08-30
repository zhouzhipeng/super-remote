#[cfg(windows)]
mod windows_input;

use remote_protocol::input::TimedInputEvent;

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
