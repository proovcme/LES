//! Physical-page table provenance, cache and literal checks.
use super::*;
pub(super) fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn pdf_tables(path: &Path, sha: &str, count: u64) -> Result<Option<Value>> {
    let Some(asset) = asset()? else {
        return Ok(None);
    };
    if !valid_hash(sha) {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    let root = folder(&state(None)?, "storage/parser-tables")?;
    let cache = root.join(format!("{sha}-{}.json", asset.hash));
    let value = if cache.exists() {
        serde_json::from_slice(&bounded(&cache, 24 * 1024 * 1024)?)
            .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?
    } else {
        let value = invoke(
            json!({"schema":"les.document-parsers.v1","action":"pdf_tables","path":path,"sha256":sha}),
        )?;
        validate_pdf(&value, sha, count)?;
        let bytes = serde_json::to_vec(&value).map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
        let temp = temporary(&root, &bytes)?;
        if fs::rename(&temp.0, &cache).is_err() && !cache.exists() {
            return Err("DOCUMENT_PARSER_CACHE_INVALID");
        }
        value
    };
    validate_pdf(&value, sha, count)?;
    Ok(Some(value))
}
fn validate_pdf(value: &Value, sha: &str, count: u64) -> Result<()> {
    if value["source_sha256"] != sha
        || value["page_count"].as_u64() != Some(count)
        || value["extractor"] != "xberg.1.3.10.pdf-tables.v1"
    {
        return Err("XBERG_PDF_TABLES_INVALID");
    }
    let tables = value["tables"]
        .as_array()
        .ok_or("XBERG_PDF_TABLES_INVALID")?;
    for t in tables {
        if t["page"].as_u64().is_none_or(|p| p == 0 || p > count) || !t["cells"].is_array() {
            return Err("XBERG_PDF_TABLES_INVALID");
        }
    }
    Ok(())
}

pub(crate) fn pdf_page_tables(
    sha: &str,
    count: u64,
    page: u64,
    snapshot: impl FnOnce() -> Result<Vec<u8>>,
) -> Result<Value> {
    let asset = asset()?.ok_or("DOCUMENT_PARSER_UNAVAILABLE")?;
    if !valid_hash(sha) || page == 0 || page > count {
        return Err("PDF_PAGE_OUT_OF_RANGE");
    }
    let root = folder(&state(None)?, "storage/parser-tables")?;
    let full = root.join(format!("{sha}-{}.json", asset.hash));
    if full.exists() {
        let value = serde_json::from_slice(&bounded(&full, 24 * 1024 * 1024)?)
            .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
        validate_pdf(&value, sha, count)?;
        return Ok(value);
    }
    let cache = root.join(format!("{sha}-{}-p{page}.json", asset.hash));
    if cache.exists() {
        let value = serde_json::from_slice(&bounded(&cache, 24 * 1024 * 1024)?)
            .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
        validate_pdf(&value, sha, count)?;
        return Ok(value);
    }
    let bytes = snapshot()?;
    let hash = les_core::sha256_hex(&bytes);
    let scratch = folder(&state(None)?, "storage/parser-tmp")?;
    let file = temporary(&scratch, &bytes)?;
    let mut value = invoke(
        json!({"schema":"les.document-parsers.v1","action":"pdf_tables","path":file.0,"sha256":hash}),
    )?;
    validate_pdf(&value, &hash, 1)?;
    for table in value["tables"]
        .as_array_mut()
        .ok_or("XBERG_PDF_TABLES_INVALID")?
    {
        table["page"] = json!(page);
    }
    value["snapshot_sha256"] = json!(hash);
    value["source_sha256"] = json!(sha);
    value["page_count"] = json!(count);
    value["physical_page"] = json!(page);
    let temp = temporary(
        &root,
        &serde_json::to_vec(&value).map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?,
    )?;
    if fs::rename(&temp.0, &cache).is_err() && !cache.exists() {
        return Err("DOCUMENT_PARSER_CACHE_INVALID");
    }
    Ok(value)
}
fn is_atom_char(c: char) -> bool {
    c.is_alphanumeric() || ".,−-+/%_".contains(c)
}
fn normalize_match_text(text: &str) -> String {
    let chars: Vec<char> = text
        .chars()
        .filter(|&c| c != '\u{00ad}' && c != '\u{200b}')
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '\u{00a0}' | '\u{2009}' | '\u{202f}') {
            let prev_digit = out.chars().next_back().is_some_and(|p| p.is_ascii_digit());
            let next_digit = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
            if !(prev_digit && next_digit) {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if c == '\u{2212}' {
            out.push('-');
            if chars
                .get(i + 1)
                .is_some_and(|&s| matches!(s, ' ' | '\u{00a0}' | '\u{2009}' | '\u{202f}'))
                && chars.get(i + 2).is_some_and(|n| n.is_ascii_digit())
            {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if matches!(c, '\u{2013}' | '\u{2014}') {
            out.push('-');
            i += 1;
            continue;
        }
        if c == 'E'
            && out.chars().next_back().is_some_and(|p| p.is_ascii_digit())
            && chars
                .get(i + 1)
                .is_some_and(|&s| matches!(s, '+' | '-' | '\u{2212}'))
            && chars.get(i + 2).is_some_and(|n| n.is_ascii_digit())
        {
            out.push('e');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}
fn literal_atoms(text: &str) -> std::collections::HashSet<String> {
    // PDF content-stream order may be column-major. Verify literal atoms on the
    // same page, including decimal/sign spelling; geometry owns their arrangement.
    text.split(|c: char| !is_atom_char(c))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn raw_contains_spaced_bounded(raw: &str, atom: &str) -> bool {
    let atom_chars: Vec<char> = atom.chars().collect();
    if atom_chars.is_empty() {
        return true;
    }
    let has_digit = atom_chars.iter().any(|c| c.is_numeric());
    let pure_alpha = atom_chars.iter().all(|c| c.is_alphabetic());
    let first = atom_chars[0];
    let last = *atom_chars.last().unwrap();
    for (start, ch) in raw.char_indices() {
        if ch != first {
            continue;
        }
        if let Some(prev) = raw[..start].chars().next_back() {
            if is_atom_char(prev) && (has_digit || !first.is_alphabetic() || !prev.is_alphabetic())
            {
                continue;
            }
        }
        let mut cursor = start + ch.len_utf8();
        let mut prev_char = ch;
        let mut matched = true;
        for &target in &atom_chars[1..] {
            let rest = &raw[cursor..];
            if prev_char.is_alphabetic() && target != '-' {
                if let Some(after) = rest
                    .strip_prefix("-\r\n")
                    .or_else(|| rest.strip_prefix("-\n"))
                {
                    if !after.starts_with(char::is_whitespace) {
                        cursor = raw.len() - after.len();
                    }
                }
            }
            let rest = &raw[cursor..];
            if prev_char == '-' {
                if let Some(after) = rest
                    .strip_prefix("\r\n")
                    .or_else(|| rest.strip_prefix('\n'))
                {
                    if !after.starts_with(char::is_whitespace) {
                        cursor = raw.len() - after.len();
                    }
                }
            } else if pure_alpha {
                if let Some(after) = rest
                    .strip_prefix("\r\n")
                    .or_else(|| rest.strip_prefix('\n'))
                {
                    if !after.starts_with(char::is_whitespace) {
                        cursor = raw.len() - after.len();
                    }
                }
            }
            let rest = &raw[cursor..];
            if !(prev_char.is_numeric() && target.is_numeric()) {
                if let Some(after) = rest.strip_prefix(' ') {
                    if !after.starts_with(char::is_whitespace) {
                        cursor = raw.len() - after.len();
                    }
                }
            }
            if let Some(next) = raw[cursor..].chars().next() {
                if next == target {
                    cursor += next.len_utf8();
                    prev_char = target;
                    continue;
                }
            }
            matched = false;
            break;
        }
        if !matched {
            continue;
        }
        if let Some(next) = raw[cursor..].chars().next() {
            if is_atom_char(next) && (has_digit || !last.is_alphabetic() || !next.is_alphabetic()) {
                continue;
            }
        }
        return true;
    }
    false
}
pub(crate) fn page_view(raw: &str, page: u64, tables: Option<&Value>) -> Result<Option<String>> {
    let Some(tables) = tables else {
        return Ok(None);
    };
    let entries = tables["tables"]
        .as_array()
        .ok_or("XBERG_PDF_TABLES_INVALID")?;
    let normalized_raw = normalize_match_text(raw);
    let hay = literal_atoms(&normalized_raw);
    let mut out = raw.to_owned();
    let mut found = false;
    for table in entries.iter().filter(|t| t["page"].as_u64() == Some(page)) {
        let rows = table["cells"]
            .as_array()
            .ok_or("XBERG_PDF_TABLES_INVALID")?;
        let columns = rows
            .iter()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .max()
            .unwrap_or(0);
        if columns == 0 {
            continue;
        }
        found = true;
        out.push_str("\n\n");
        for (index, row) in rows.iter().enumerate() {
            let cells = row.as_array().ok_or("XBERG_PDF_TABLES_INVALID")?;
            out.push('|');
            for column in 0..columns {
                let text = cells
                    .get(column)
                    .map_or(Some(""), Value::as_str)
                    .ok_or("XBERG_PDF_TABLES_INVALID")?;
                let normalized_cell = normalize_match_text(text);
                if literal_atoms(&normalized_cell).iter().any(|atom| {
                    !hay.contains(atom)
                        && !raw_contains_spaced_bounded(&normalized_raw, atom)
                        && !(atom.chars().all(|c| !c.is_alphanumeric())
                            && normalized_raw.contains(atom))
                }) {
                    return Err("PDF_TABLE_SOURCE_MISMATCH");
                }
                out.push(' ');
                out.push_str(
                    &text
                        .replace('&', "&amp;")
                        .replace('|', "&#124;")
                        .replace('\n', "<br>"),
                );
                out.push_str(" |");
            }
            out.push('\n');
            if index == 0 {
                out.push('|');
                for _ in 0..columns {
                    out.push_str(" --- |");
                }
                out.push('\n');
            }
        }
        if out.len() > 12 * 1024 * 1024 {
            return Err("PDF_TEXT_LIMIT");
        }
    }
    Ok(found.then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pdf_tables_preserve_cells_and_physical_page_without_changing_numbers() {
        let value = json!({"tables":[{"page":2,"cells":[["Position","Quantity"],["Alpha item","-17.25"],["Beta | item","0"]]}]});
        assert!(page_view("no table here", 1, Some(&value))
            .unwrap()
            .is_none());
        let text = page_view(
            "Position Quantity Alpha item -17.25 Beta | item 0",
            2,
            Some(&value),
        )
        .unwrap()
        .unwrap();
        assert!(text.contains("| Alpha item | -17.25 |"));
        assert!(text.contains("| Beta &#124; item | 0 |"));
        assert!(!text.contains("23.50"));
    }
    #[test]
    fn ungrounded_cell_fails_instead_of_becoming_an_authoritative_table() {
        let value = json!({"tables":[{"page":1,"cells":[["Alpha","99.99"]]}]});
        assert_eq!(
            page_view("Alpha 17.25", 1, Some(&value)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
    }
    #[test]
    fn alphabetic_spacing_differences_do_not_relax_numbers_or_invent_words() {
        let table = |cell: &str| json!({"tables":[{"page":1,"cells":[[cell,"17.25"]]}]});
        assert!(page_view(
            "Technical description 17.25",
            1,
            Some(&table("Tech nical description"))
        )
        .unwrap()
        .is_some());
        assert!(page_view(
            "Technical description 17.25",
            1,
            Some(&table("Invented description"))
        )
        .is_err());
        assert!(page_view(
            "Technical description 117.25",
            1,
            Some(&table("Technical description"))
        )
        .is_err());
    }
    #[test]
    fn column_major_stream_and_wrapped_cell_do_not_lose_geometry() {
        let value = json!({"tables":[{"page":1,"cells":[["Item","Quantity"],["Alpha item continued description","17.25"],["Beta item","23.50"]]}]});
        let text = page_view(
            "Item Alpha item Beta item Quantity 17.25 23.50 continued description",
            1,
            Some(&value),
        )
        .unwrap()
        .unwrap();
        assert!(text.contains("| Alpha item continued description | 17.25 |"));
    }
    #[test]
    fn kerning_and_unicode_dash_normalization_preserve_numeric_bounds() {
        let value = json!({"tables":[{"page":1,"cells":[["Труба DN100 – L\u{00a0}=\u{00a0}2500","17.25"]]}]});
        assert!(
            page_view("Труба DN 100 — L = 2\u{00a0}500 17.25", 1, Some(&value))
                .unwrap()
                .is_some()
        );
        let bad_number = json!({"tables":[{"page":1,"cells":[["Труба DN100","17.25"]]}]});
        assert_eq!(
            page_view("Труба DN 100 117.25", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Труба DN 100 17.250", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Труба DN 100 −17.25", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Труба DN 100 -17.25", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Труба DN 100 − 17.25", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Труба DN 100 +17.25", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Труба DN 100 17.25%", 1, Some(&bad_number)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        let glued_lines = json!({"tables":[{"page":1,"cells":[["Позиция","2500"]]}]});
        assert_eq!(
            page_view("Позиция\n2\n500", 1, Some(&glued_lines)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Позиция\r\n2\r\n500", 1, Some(&glued_lines)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        assert_eq!(
            page_view("Позиция 2 500", 1, Some(&glued_lines)).unwrap_err(),
            "PDF_TABLE_SOURCE_MISMATCH"
        );
        let signed = json!({"tables":[{"page":1,"cells":[["Позиция","-17.25"]]}]});
        assert!(page_view("Позиция −17.25", 1, Some(&signed))
            .unwrap()
            .is_some());
        assert!(page_view("Позиция − 17.25", 1, Some(&signed))
            .unwrap()
            .is_some());
    }
    #[test]
    fn cached_tables_are_bound_to_exact_revision_and_page_count() {
        let sha = "a".repeat(64);
        let value = json!({"source_sha256":sha,"page_count":2,"extractor":"xberg.1.3.10.pdf-tables.v1","tables":[]});
        assert!(validate_pdf(&value, &sha, 2).is_ok());
        assert!(validate_pdf(&value, &"b".repeat(64), 2).is_err());
        assert!(validate_pdf(&value, &sha, 1).is_err());
    }
}
