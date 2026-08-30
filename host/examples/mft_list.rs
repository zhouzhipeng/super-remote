#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use windows::Win32::{
        Media::MediaFoundation::*,
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut activates,
            &mut count,
        )?;
        println!("hardware H.264 MFT count: {count}");
        for activation in std::slice::from_raw_parts(activates, count as usize)
            .iter()
            .flatten()
        {
            let mut name = [0u16; 256];
            let mut length = 0;
            activation.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name, Some(&mut length))?;
            println!("- {}", String::from_utf16_lossy(&name[..length as usize]));
        }
        CoTaskMemFree(Some(activates.cast()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
