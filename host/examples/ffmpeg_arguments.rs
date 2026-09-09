// Let the pixel regression test consume the exact production encoder arguments.
#[path = "../src/ffmpeg_options.rs"]
mod ffmpeg_options;

fn main() {
    println!(
        "{}",
        serde_json::to_string(&ffmpeg_options::encoding_args("h264_nvenc", 20_000_000, 60))
            .unwrap()
    );
}
