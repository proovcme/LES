use serde_json::{json, Value};
use std::{fs::File, io::Read, path::Path};

const MAX_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn decode(bytes: &[u8]) -> Option<String> {
    let utf32 = bytes
        .strip_prefix(&[0xff, 0xfe, 0, 0])
        .map(|data| (data, true))
        .or_else(|| {
            bytes
                .strip_prefix(&[0, 0, 0xfe, 0xff])
                .map(|data| (data, false))
        });
    if let Some((data, little)) = utf32 {
        let (units, remainder) = data.as_chunks::<4>();
        if !remainder.is_empty() {
            return None;
        }
        return units
            .iter()
            .map(|unit| {
                char::from_u32(if little {
                    u32::from_le_bytes(*unit)
                } else {
                    u32::from_be_bytes(*unit)
                })
            })
            .collect();
    }
    let utf16 = bytes
        .strip_prefix(&[0xff, 0xfe])
        .map(|data| (data, true))
        .or_else(|| bytes.strip_prefix(&[0xfe, 0xff]).map(|data| (data, false)));
    if let Some((data, little)) = utf16 {
        let (units, remainder) = data.as_chunks::<2>();
        if !remainder.is_empty() {
            return None;
        }
        return char::decode_utf16(units.iter().map(|unit| {
            if little {
                u16::from_le_bytes(*unit)
            } else {
                u16::from_be_bytes(*unit)
            }
        }))
        .collect::<Result<String, _>>()
        .ok();
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

pub(super) fn read(path: &Path) -> Result<Value, &'static str> {
    let file = File::open(path).map_err(|_| "DOCUMENT_READ_FAILED")?;
    let metadata = file.metadata().map_err(|_| "DOCUMENT_READ_FAILED")?;
    if !metadata.is_file() {
        return Err("DOCUMENT_READ_FAILED");
    }
    if metadata.len() > MAX_BYTES {
        return Ok(json!({"status":"compatibility"}));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "DOCUMENT_READ_FAILED")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Ok(json!({"status":"compatibility"}));
    }
    let Some(text) = decode(&bytes) else {
        // Legacy detection and precise damaged-BOM errors stay in the compatibility reader.
        return Ok(json!({"status":"compatibility"}));
    };
    if text
        .chars()
        .any(|c| c < ' ' && !matches!(c, '\t' | '\r' | '\n' | '\u{c}'))
    {
        return Ok(json!({"status":"binary"}));
    }
    Ok(json!({"status":"completed","text":text,"extractor":"les.unicode.v1"}))
}
