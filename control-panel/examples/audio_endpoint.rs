//! Local diagnostic: inspect or mute the same console endpoint used by policy.
use windows::Win32::{
    Media::Audio::{
        Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
    },
    System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    },
};
fn main() -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let id = device.GetId()?;
        let name = id.to_string()?;
        CoTaskMemFree(Some(id.0.cast()));
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        if std::env::args().any(|arg| arg == "--mute") {
            volume.SetMute(true, std::ptr::null())?;
        }
        println!(
            "{}",
            serde_json::json!({"endpoint":name,"muted":volume.GetMute()?.as_bool()})
        );
        drop(volume);
        drop(device);
        drop(enumerator);
        CoUninitialize();
    }
    Ok(())
}
