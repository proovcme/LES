//! Bounded raster decoding and visible Word-picture transforms. Pixels are opaque BGRA.
use image::{imageops, ImageBuffer, ImageFormat, ImageReader, Limits, Rgba};
use serde_json::Value;
use std::io::Cursor;
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
pub fn preview_png(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>> {
    if width == 0
        || height == 0
        || width as u64 * height as u64 > 25_000_000
        || bgra.len() as u64 != width as u64 * height as u64 * 4
    {
        return Err(BAD);
    }
    let source = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, bgra).ok_or(BAD)?;
    let scale = (1800. / width.max(height) as f64).min(1.);
    let mut image = imageops::resize(
        &source,
        (width as f64 * scale).round().max(1.) as u32,
        (height as f64 * scale).round().max(1.) as u32,
        imageops::FilterType::Triangle,
    );
    for p in image.pixels_mut() {
        p.0.swap(0, 2);
    }
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|_| BAD)?;
    Ok(out.into_inner())
}
type Result<T> = std::result::Result<T, &'static str>;
const BAD: &str = "DOCX_IMAGE_INVALID";
pub fn decode(bytes: &[u8]) -> Result<Pixels> {
    let format = image::guess_format(bytes).map_err(|_| BAD)?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Bmp
    ) {
        return Err("DOCX_IMAGE_UNSUPPORTED");
    }
    let (w, h) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| BAD)?;
    if w == 0 || h == 0 || w as u64 * h as u64 > 25_000_000 {
        return Err("DOCX_IMAGE_LIMIT");
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits.max_image_width = Some(w);
    limits.max_image_height = Some(h);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| BAD)?.to_rgba8();
    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
    for p in image.pixels() {
        let a = p[3] as u32;
        for c in [p[2], p[1], p[0]] {
            bgra.push(((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8);
        }
        bgra.push(255);
    }
    Ok(Pixels {
        width: w,
        height: h,
        bgra,
    })
}
pub fn transform(pixels: Pixels, options: &Value) -> Result<Pixels> {
    if pixels.width == 0
        || pixels.height == 0
        || pixels.width as u64 * pixels.height as u64 > 25_000_000
    {
        return Err(BAD);
    }
    let mut image = ImageBuffer::<Rgba<u8>, _>::from_raw(pixels.width, pixels.height, pixels.bgra)
        .ok_or(BAD)?;
    let mut crops = [0f64; 4];
    for (index, key) in ["l", "t", "r", "b"].into_iter().enumerate() {
        if let Some(value) = options["picture"]["crop"][key].as_str() {
            crops[index] = value.parse::<f64>().map_err(|_| BAD)? / 100000.;
        } else if let Some(value) = options["attributes"]
            [format!("crop{}", ["left", "top", "right", "bottom"][index])]
        .as_str()
        {
            crops[index] = if let Some(s) = value.strip_suffix('f') {
                s.parse::<f64>().map_err(|_| BAD)? / 65536.
            } else {
                value.parse::<f64>().map_err(|_| BAD)?
            };
        }
        if !crops[index].is_finite() || !(-1.0..1.0).contains(&crops[index]) {
            return Err(BAD);
        }
    }
    let (w, h) = image.dimensions();
    let x = (w as f64 * crops[0].max(0.)).round() as u32;
    let y = (h as f64 * crops[1].max(0.)).round() as u32;
    let right = (w as f64 * (1. - crops[2].max(0.))).round() as u32;
    let bottom = (h as f64 * (1. - crops[3].max(0.))).round() as u32;
    if right <= x || bottom <= y {
        return Err(BAD);
    }
    if x != 0 || y != 0 || right != w || bottom != h {
        image = imageops::crop_imm(&image, x, y, right - x, bottom - y).to_image();
    }
    let transform = &options["picture"]["transform"];
    if matches!(transform["flipH"].as_str(), Some("1" | "true")) {
        imageops::flip_horizontal_in_place(&mut image);
    }
    if matches!(transform["flipV"].as_str(), Some("1" | "true")) {
        imageops::flip_vertical_in_place(&mut image);
    }
    let rotation = transform["rot"]
        .as_str()
        .unwrap_or("0")
        .parse::<i64>()
        .map_err(|_| BAD)?
        .rem_euclid(21600000);
    image = match rotation {
        0 => image,
        5400000 => imageops::rotate90(&image),
        10800000 => imageops::rotate180(&image),
        16200000 => imageops::rotate270(&image),
        _ => return Err("DOCX_IMAGE_TRANSFORM_UNSUPPORTED"),
    };
    Ok(Pixels {
        width: image.width(),
        height: image.height(),
        bgra: image.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn crop_rotation_and_preview_preserve_color_and_reject_invalid_geometry() {
        let pixels = Pixels {
            width: 2,
            height: 1,
            bgra: vec![0, 0, 255, 255, 255, 0, 0, 255],
        };
        let p = transform(
            pixels,
            &json!({"picture":{"crop":{"l":"50000"},"transform":{"rot":"5400000"}}}),
        )
        .unwrap();
        assert_eq!((p.width, p.height), (1, 1));
        assert_eq!(p.bgra, [255, 0, 0, 255]);
        let encoded = preview_png(p.width, p.height, &p.bgra).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.bgra, p.bgra);
        assert!(preview_png(0, 0, &[]).is_err());
        assert!(preview_png(2, 2, &[0; 4]).is_err());
        let p = Pixels {
            width: 2,
            height: 1,
            bgra: vec![0; 8],
        };
        assert!(transform(p, &json!({"picture":{"crop":{"l":"75000","r":"75000"}}})).is_err());
        assert!(decode(b"corrupt image").is_err());
    }
    #[test]
    fn transparent_image_is_composited_on_white_without_black_background() {
        let rgba = ImageBuffer::<Rgba<u8>, _>::from_raw(1, 1, vec![255, 0, 0, 0]).unwrap();
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        assert_eq!(decode(out.get_ref()).unwrap().bgra, [255, 255, 255, 255]);
    }
    #[test]
    fn native_bitmap_pixels_keep_dimensions_and_channels() {
        let rgb = image::RgbImage::from_raw(2, 1, vec![255, 0, 0, 0, 0, 255]).unwrap();
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(rgb)
            .write_to(&mut out, ImageFormat::Bmp)
            .unwrap();
        let pixels = decode(out.get_ref()).unwrap();
        assert_eq!((pixels.width, pixels.height), (2, 1));
        assert_eq!(pixels.bgra, [0, 0, 255, 255, 255, 0, 0, 255]);
    }
}
