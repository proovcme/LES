//! Decode before admission, with a fixed byte/pixel/allocation budget. No network URLs.
use super::*;
use image::{ImageFormat, ImageReader, Limits};
use std::io::Cursor;
pub(super) fn bytes(raw: &[u8]) -> Result<Value> {
    if raw.len() > 10 * 1024 * 1024 {
        return Err("ATTACHMENT_IMAGE_LIMIT");
    }
    let format = image::guess_format(raw).map_err(|_| "ATTACHMENT_IMAGE_INVALID")?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => return Err("ATTACHMENT_IMAGE_UNSUPPORTED"),
    };
    let (w, h) = ImageReader::with_format(Cursor::new(raw), format)
        .into_dimensions()
        .map_err(|_| "ATTACHMENT_IMAGE_INVALID")?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 25_000_000 {
        return Err("ATTACHMENT_IMAGE_PIXELS");
    }
    let mut reader = ImageReader::with_format(Cursor::new(raw), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits.max_image_width = Some(w);
    limits.max_image_height = Some(h);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| "ATTACHMENT_IMAGE_INVALID")?;
    if decoded.width() != w || decoded.height() != h {
        return Err("ATTACHMENT_IMAGE_INVALID");
    }
    Ok(
        json!({"data_url":format!("data:{mime};base64,{}",base64(raw)),"mime":mime,"width":w,"height":h,"source_sha256":crate::sha256_hex(raw)}),
    )
}
pub(crate) fn base64(raw: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
    for group in raw.chunks(3) {
        let a = group[0];
        let b = group.get(1).copied().unwrap_or(0);
        let c = group.get(2).copied().unwrap_or(0);
        out.push(CHARS[(a >> 2) as usize] as char);
        out.push(CHARS[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if group.len() > 1 {
            CHARS[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if group.len() > 2 {
            CHARS[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
pub fn file(path: &Path) -> Result<Value> {
    let f = File::open(path).map_err(|_| "ATTACHMENT_READ_FAILED")?;
    if !f
        .metadata()
        .map_err(|_| "ATTACHMENT_READ_FAILED")?
        .is_file()
    {
        return Err("ATTACHMENT_IMAGE_INVALID");
    }
    let mut raw = Vec::new();
    f.take(10 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "ATTACHMENT_READ_FAILED")?;
    bytes(&raw)
}
pub(super) fn asset(mut asset: Asset) -> Result<Value> {
    let mut raw = Vec::new();
    Read::by_ref(&mut asset.file)
        .take(10 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "ATTACHMENT_READ_FAILED")?;
    if raw.len() as f64 != number(&asset.metadata["size"])?
        || Some(crate::sha256_hex(&raw).as_str()) != asset.metadata["sha256"].as_str()
    {
        return Err("ATTACHMENT_HASH_CHANGED");
    }
    bytes(&raw)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoding_has_only_standard_padding() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output);
        }
    }
}
