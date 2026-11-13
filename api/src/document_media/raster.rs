use super::{vector, Result};
use serde_json::Value;
pub(super) fn render(bytes: &[u8], options: &Value) -> Result<(u32, u32, Vec<u8>)> {
    let pixels = if bytes.starts_with(&[0xd7, 0xcd, 0xc6, 0x9a])
        || bytes.starts_with(&[1, 0, 0, 0])
        || bytes.get(2..4) == Some(&[9, 0])
    {
        vector::render(bytes)?
    } else {
        les_core::document_image::decode(bytes)?
    };
    let pixels = les_core::document_image::transform(pixels, options)?;
    Ok((pixels.width, pixels.height, pixels.bgra))
}
