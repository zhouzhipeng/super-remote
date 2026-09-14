// Shared by the Host and the local clipboard HTTP bridge.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
#[cfg(windows)]
pub fn read_png() -> anyhow::Result<Option<Vec<u8>>> {
    let image = match arboard::Clipboard::new()?.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(image.bytes.len() <= 64 * 1024 * 1024, "剪贴板图片像素过大");
    let mut png = std::io::Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut png,
        &image.bytes,
        image.width as u32,
        image.height as u32,
        image::ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )?;
    let png = png.into_inner();
    anyhow::ensure!(png.len() <= MAX_IMAGE_BYTES, "剪贴板图片超过 16 MiB");
    Ok(Some(png))
}
#[cfg(windows)]
pub fn write_png(bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(bytes.len() <= MAX_IMAGE_BYTES, "剪贴板图片超过 16 MiB");
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.into_rgba8();
    anyhow::ensure!(
        image.as_raw().len() <= 64 * 1024 * 1024,
        "image pixels exceed 64 MiB"
    );
    arboard::Clipboard::new()?.set_image(arboard::ImageData {
        width: image.width() as usize,
        height: image.height() as usize,
        bytes: std::borrow::Cow::Owned(image.into_raw()),
    })?;
    Ok(())
}
#[cfg(not(windows))]
pub fn read_png() -> anyhow::Result<Option<Vec<u8>>> {
    Ok(None)
}
#[cfg(not(windows))]
pub fn write_png(_: &[u8]) -> anyhow::Result<()> {
    anyhow::bail!("Windows required")
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn rejects_invalid_png_without_touching_clipboard() {
        assert!(super::write_png(b"not a png").is_err());
    }
    #[test]
    fn rejects_oversized_image_before_decoding() {
        assert!(super::write_png(&vec![0; super::MAX_IMAGE_BYTES + 1]).is_err());
    }
}
