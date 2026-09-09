/// Keep desktop text at the same quantization on IDR and predicted frames.
/// NVENC's CBR/VBR rate controller periodically requantizes even a static screen.
/// Constant QP trades a bitrate ceiling for stable detail; AQ must also be off.
pub fn fixed_qp(encoder: &str) -> Option<u8> {
    (encoder == "h264_nvenc").then_some(18)
}

pub fn encoding_args(encoder: &str, bitrate: u32, fps: u16) -> Vec<String> {
    let mut args = vec!["-c:v".to_owned(), encoder.to_owned()];
    let profile = match encoder {
        "h264_nvenc" => {
            args.extend(
                [
                    "-rc",
                    "constqp",
                    "-qp",
                    "18",
                    "-preset",
                    "p4",
                    "-tune",
                    "ull",
                    "-delay",
                    "0",
                    "-surfaces",
                    "2",
                    "-zerolatency",
                    "1",
                    "-forced-idr",
                    "1",
                    "-rc-lookahead",
                    "0",
                    "-spatial-aq",
                    "0",
                    "-temporal-aq",
                    "0",
                    "-strict_gop",
                    "1",
                    "-slices",
                    "1",
                ]
                .map(str::to_owned),
            );
            "baseline"
        }
        "h264_amf" => {
            args.extend(
                ["-rc", "cbr", "-quality", "speed", "-usage", "lowlatency"].map(str::to_owned),
            );
            "constrained_baseline"
        }
        "libx264" => {
            args.extend(
                [
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-x264-params",
                    "scenecut=0:rc-lookahead=0:sync-lookahead=0",
                ]
                .map(str::to_owned),
            );
            "baseline"
        }
        _ => unreachable!("validated by HostConfig::load"),
    };
    if fixed_qp(encoder).is_none() {
        args.extend([
            "-b:v".into(),
            bitrate.to_string(),
            "-maxrate".into(),
            bitrate.to_string(),
            "-bufsize".into(),
            bitrate
                .div_ceil(u32::from(fps))
                .saturating_mul(4)
                .to_string(),
        ]);
    }
    // Retain periodic recovery points. Removing IDRs merely hides the quality
    // pulse and prevents recovery after reference-frame loss.
    args.extend([
        "-profile:v".into(),
        profile.into(),
        "-g".into(),
        (u32::from(fps) * 2).to_string(),
        "-bf".into(),
        "0".into(),
    ]);
    args
}
