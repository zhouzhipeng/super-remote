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

/// Rate-controlled motion path used while lossless tiles restore exact pixels
/// after input settles. The ceiling is whatever the caller already resolved
/// (`HostConfig::interaction_video`); a second, lower limit here silently
/// capped the interaction stream no matter how the deployment was configured.
pub fn hybrid_encoding_args(encoder: &str, bitrate: u32, fps: u16) -> Vec<String> {
    let ceiling = bitrate.max(1);
    let mut args = encoding_args(encoder, ceiling, fps);
    if encoder == "h264_nvenc" {
        let rc = args.iter().position(|arg| arg == "-rc").unwrap();
        args[rc + 1] = "vbr".into();
        let qp = args.iter().position(|arg| arg == "-qp").unwrap();
        args[qp] = "-cq".into();
        args.extend([
            "-b:v".into(),
            (ceiling / 2).to_string(),
            "-maxrate".into(),
            ceiling.to_string(),
            // VBV buffer, in frames at the ceiling. Two frames forced the rate
            // controller to fit a scroll - full-frame motion over detailed text,
            // the most expensive thing a desktop encoder sees - inside twice the
            // average frame, so quality collapsed frame by frame for exactly as
            // long as the user was scrolling. Six absorbs that burst and still
            // bounds the queue it can build on a relay to about 100 ms.
            "-bufsize".into(),
            ceiling
                .div_ceil(u32::from(fps))
                .saturating_mul(6)
                .to_string(),
        ]);
    }
    args
}

#[cfg(test)]
mod tests {
    #[test]
    fn hybrid_nvenc_honours_the_caller_ceiling_without_changing_legacy_quality() {
        let legacy = super::encoding_args("h264_nvenc", 20_000_000, 60);
        assert!(legacy.iter().any(|arg| arg == "constqp"));
        let args = super::hybrid_encoding_args("h264_nvenc", 6_000_000, 60);
        let value = |key| &args[args.iter().position(|arg| arg == key).unwrap() + 1];
        assert_eq!(value("-rc"), "vbr");
        assert_eq!(value("-cq"), "18");
        assert_eq!(value("-b:v"), "3000000");
        assert_eq!(value("-maxrate"), "6000000");
        assert_eq!(value("-bufsize"), "600000");
        assert_eq!(value("-bf"), "0");
        // No hidden ceiling: a deployment configured for more gets more.
        let wide = super::hybrid_encoding_args("h264_nvenc", 20_000_000, 60);
        assert_eq!(wide[wide.iter().position(|arg| arg == "-maxrate").unwrap() + 1], "20000000");
    }
}
