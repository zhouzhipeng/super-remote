// Let the pixel regression test consume the exact production encoder arguments.
#[path = "../src/ffmpeg_options.rs"]
mod ffmpeg_options;

fn main() {
    let encode = if std::env::args().any(|arg| arg == "--hybrid") {
        ffmpeg_options::hybrid_encoding_args
    } else {
        ffmpeg_options::encoding_args
    };
    println!(
        "{}",
        serde_json::to_string(&encode("h264_nvenc", 20_000_000, 60)).unwrap()
    );
}
