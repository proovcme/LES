use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

const LIMIT: usize = 15 * 1024 * 1024;

pub fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<String, &'static str> {
    if rgb.len() > LIMIT
        || ((width as u64) * (height as u64)).checked_mul(3) != Some(rgb.len() as u64)
    {
        return Err("PDF_PREVIEW_LIMIT");
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(rgb, width, height, ExtendedColorType::Rgb8)
        .map_err(|_| "PDF_PREVIEW_FAILED")?;
    if png.len() > LIMIT {
        return Err("PDF_PREVIEW_LIMIT");
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(png.len() * 2);
    for byte in png {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(encoded)
}

pub fn decode_png(encoded: &str) -> Result<Vec<u8>, &'static str> {
    if encoded.len() > LIMIT * 2 || !encoded.len().is_multiple_of(2) {
        return Err("PDF_PREVIEW_LIMIT");
    }
    let hex = |byte| match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("PDF_PREVIEW_FAILED"),
    };
    let bytes = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok::<u8, &'static str>(hex(pair[0])? * 16 + hex(pair[1])?))
        .collect::<Result<Vec<_>, _>>()?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("PDF_PREVIEW_FAILED");
    }
    Ok(bytes)
}
