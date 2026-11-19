//! Long physical pages use the existing structural reader, not a second size cutoff.
use serde_json::{json, Value};

pub(super) fn reading_units(full: &Value) -> Result<Vec<Value>, &'static str> {
    let text = full["text"].as_str().ok_or("PDF_TEXT_UNAVAILABLE")?;
    if full["total_chars"] != text.chars().count()
        || full["view_sha256"] != les_core::sha256_hex(text.as_bytes())
    {
        return Err("SOURCE_VIEW_CHANGED");
    }
    if text.len() > les_core::context_selection::MAX_SOURCE_TEXT_BYTES {
        return Err("SOURCE_READ_LIMIT");
    }
    if text.chars().count() <= 16000 {
        return Ok(vec![full.clone()]);
    }
    let view =
        les_core::document_reading(&json!({"text":text,"max_chars":4000,"for_context":true}))?;
    let units = view["units"]
        .as_array()
        .ok_or("DOCUMENT_STRUCTURE_INVALID")?;
    if units.is_empty() || units.len() > les_core::context_selection::MAX_SOURCE_UNITS {
        return Err("SOURCE_READ_LIMIT");
    }
    let mut bytes = 0usize;
    let (mut previous_byte, mut read_offset) = (0usize, 0usize);
    let mut result = Vec::with_capacity(units.len());
    for unit in units {
        let body = unit["text"].as_str().ok_or("DOCUMENT_STRUCTURE_INVALID")?;
        bytes = bytes.checked_add(body.len()).ok_or("SOURCE_READ_LIMIT")?;
        if bytes > les_core::context_selection::MAX_SOURCE_TEXT_BYTES {
            return Err("SOURCE_READ_LIMIT");
        }
        let start = unit["start_byte"]
            .as_u64()
            .ok_or("DOCUMENT_STRUCTURE_INVALID")? as usize;
        let end = unit["end_byte"]
            .as_u64()
            .ok_or("DOCUMENT_STRUCTURE_INVALID")? as usize;
        if start > end
            || start < previous_byte
            || text.get(start..end).is_none()
            || !unit["ordinal"].is_u64()
        {
            return Err("DOCUMENT_STRUCTURE_INVALID");
        }
        read_offset += text[previous_byte..start].chars().count();
        previous_byte = start;
        // Copy the small snapshot identity once per unit, never the full page
        // before replacing its text. Carried table headers are not raw char
        // windows: do not give them a read_pdf_source continuation cursor.
        let mut read = Value::Object(
            full.as_object()
                .ok_or("DOCUMENT_STRUCTURE_INVALID")?
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "text" | "offset" | "next_offset"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        read["text"] = unit["text"].clone();
        read["complete"] = json!(false);
        read["selection"] = json!("structured_page_unit");
        read["reading_unit_schema"] = json!("les.pdf-reading-unit.v1");
        read["reading_unit_sha256"] = json!(les_core::sha256_hex(body.as_bytes()));
        read["read_offset"] = json!(read_offset);
        for field in [
            "ordinal",
            "start_byte",
            "end_byte",
            "heading",
            "table_id",
            "row_start",
            "row_end",
        ] {
            if !unit[field].is_null() {
                read[field] = unit[field].clone();
            }
        }
        result.push(read);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(text: &str) -> Value {
        json!({"text":text,"total_chars":text.chars().count(),"view_sha256":les_core::sha256_hex(text.as_bytes()),
            "source_sha256":"a".repeat(64),"page":7,"offset":0,"next_offset":null,"complete":true,
            "extractor":"source-checked-test","table_projection_warning":"PDF_TABLE_SOURCE_MISMATCH"})
    }
    #[test]
    fn long_tables_keep_every_row_once_and_carry_their_original_headers() {
        let header = "|Position|Unit|Quantity|\n|---|---|---|\n";
        let rows = (0..600)
            .map(|n| format!("|Valve {n} корпус 東京|pcs|{n}.250|\n"))
            .collect::<Vec<_>>();
        let text = format!("# Schedule\n{header}{}", rows.join(""));
        assert!(text.chars().count() > 16000);
        let original = page(&text);
        let units = reading_units(&original).unwrap();
        assert!(units.len() > 1);
        for row in rows {
            assert_eq!(
                units
                    .iter()
                    .filter(|u| u["text"].as_str().unwrap().contains(row.trim()))
                    .count(),
                1
            );
        }
        for unit in &units {
            assert!(unit["text"].as_str().unwrap().contains(header));
            assert_eq!(unit["source_sha256"], original["source_sha256"]);
            assert_eq!(unit["view_sha256"], original["view_sha256"]);
            assert_eq!(
                unit["reading_unit_sha256"],
                les_core::sha256_hex(unit["text"].as_str().unwrap().as_bytes())
            );
            assert_eq!(unit["page"], 7);
            assert_eq!(
                unit["read_offset"],
                text[..unit["start_byte"].as_u64().unwrap() as usize]
                    .chars()
                    .count()
            );
            assert_eq!(unit["complete"], false);
            assert_eq!(
                unit["table_projection_warning"],
                original["table_projection_warning"]
            );
            assert!(unit.get("offset").is_none() && unit.get("next_offset").is_none());
        }
        assert_eq!(
            reading_units(&page("Short unchanged page 117.250")).unwrap(),
            vec![page("Short unchanged page 117.250")]
        );
    }
    #[test]
    fn an_unbound_page_is_never_split_into_apparently_verified_units() {
        let mut input = page(&"Long paragraph 東京 17.25. ".repeat(1000));
        input["view_sha256"] = json!("b".repeat(64));
        assert_eq!(reading_units(&input).unwrap_err(), "SOURCE_VIEW_CHANGED");
        input["view_sha256"] = json!(les_core::sha256_hex(
            input["text"].as_str().unwrap().as_bytes()
        ));
        input["total_chars"] = json!(1);
        assert_eq!(reading_units(&input).unwrap_err(), "SOURCE_VIEW_CHANGED");
    }
}
