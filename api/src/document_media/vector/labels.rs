//! Literal Unicode text records, not a diagram-to-facts interpreter.
use super::{u32at, Result, BAD};
use serde_json::{json, Value};
use std::collections::HashMap;
pub(super) fn read(bytes: &[u8]) -> Result<Value> {
    super::validate(bytes)?;
    let mut at = 0;
    let mut runs = Vec::new();
    let mut complete = true;
    let mut fonts = HashMap::new();
    let mut rotation = 0i32;
    let mut stack = Vec::new();
    while at < bytes.len() {
        let kind = u32at(bytes, at)?;
        let size = u32at(bytes, at + 4)? as usize;
        let record = &bytes[at..at + size];
        match kind {
            82 if size >= 104 => {
                fonts.insert(u32at(record, 8)?, u32at(record, 20)? as i32);
            }
            37 if size >= 12 => {
                if let Some(value) = fonts.get(&u32at(record, 8)?) {
                    rotation = *value;
                }
            }
            33 => stack.push(rotation),
            34 if size >= 12 => {
                let level = u32at(record, 8)? as i32;
                let index = if level < 0 {
                    stack.len() as i64 + level as i64
                } else {
                    level as i64 - 1
                };
                if index < 0 || index as usize >= stack.len() {
                    complete = false;
                } else {
                    rotation = stack[index as usize];
                    stack.truncate(index as usize);
                }
            }
            // Glyph IDs, opaque ANSI encodings and unsupported text layouts
            // are not guessed. Bitmap/text-clipping/transform records require
            // the rendered OCR path; direct labels alone are not complete.
            26 | 28 | 29 | 30 | 35 | 36 | 67 | 75 | 76 | 77 | 78 | 79 | 80 | 81 | 83 | 96 | 97
            | 108 | 114 | 116 => complete = false,
            84 => {
                if size < 76 {
                    return Err(BAD);
                }
                let count = u32at(record, 44)? as usize;
                let offset = u32at(record, 48)? as usize;
                let flags = u32at(record, 52)?;
                if flags & 0x10 != 0 || flags & 4 != 0 {
                    complete = false;
                    at += size;
                    continue;
                }
                let length = count.checked_mul(2).ok_or(BAD)?;
                if count > 1_000_000
                    || offset < 76
                    || offset.checked_add(length).is_none_or(|n| n > size)
                {
                    return Err(BAD);
                }
                let codes = record[offset..offset + length]
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>();
                let Ok(text) = String::from_utf16(&codes) else {
                    complete = false;
                    at += size;
                    continue;
                };
                if text.chars().any(|c| {
                    ('\u{e000}'..='\u{f8ff}').contains(&c)
                        || (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                }) {
                    complete = false;
                    at += size;
                    continue;
                }
                if !text.trim().is_empty() {
                    runs.push(json!({"text":text,"x":u32at(record,36)? as i32,"y":u32at(record,40)? as i32,
                        "rotation_tenths":rotation,"options":flags,"record_offset":at}));
                }
            }
            _ => {}
        }
        if runs.len() > 65536 {
            return Err("DOCX_IMAGE_LIMIT");
        }
        at += size;
    }
    Ok(
        json!({"runs":runs,"complete":complete,"semantics":"literal_labels_only_geometry_not_interpreted"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(text: &str, flags: u32) -> Vec<u8> {
        let encoded = text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let n = (76 + encoded.len() + 3) & !3;
        let mut data = vec![0; 88 + n + 20];
        for (at, v) in [
            (0, 1),
            (4, 88),
            (40, 0x464d4520),
            (48, data.len() as u32),
            (52, 3),
            (88, 84),
            (92, n as u32),
            (88 + 44, text.encode_utf16().count() as u32),
            (88 + 48, 76),
            (88 + 52, flags),
            (88 + n, 14),
            (88 + n + 4, 20),
        ] {
            data[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        data[88 + 76..88 + 76 + encoded.len()].copy_from_slice(&encoded);
        data
    }
    #[test]
    fn exact_unicode_digits_and_signs_survive_but_glyph_ids_do_not_become_text() {
        let value = read(&fixture("Щит −17.25", 0)).unwrap();
        assert_eq!(value["runs"][0]["text"], "Щит −17.25");
        assert_eq!(value["complete"], true);
        let glyphs = read(&fixture("123", 16)).unwrap();
        assert_eq!(glyphs["complete"], false);
        assert!(glyphs["runs"].as_array().unwrap().is_empty());
        let clipped = read(&fixture("117.250", 4)).unwrap();
        assert_eq!(clipped["complete"], false);
        let unknown = read(&fixture("\u{f061}", 0)).unwrap();
        assert_eq!(unknown["complete"], false);
    }
}
