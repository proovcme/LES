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
fn literal_atoms(text: &str) -> std::collections::HashSet<String> {
    // PDF content-stream order may be column-major. Verify literal atoms on the
    // same page, including decimal/sign spelling; geometry owns their arrangement.
    text.split(|c: char| !(c.is_alphanumeric() || ".,−-+/%_".contains(c)))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
pub(crate) fn page_view(raw: &str, page: u64, tables: Option<&Value>) -> Result<Option<String>> {
    let Some(tables) = tables else {
        return Ok(None);
    };
    let entries = tables["tables"]
        .as_array()
        .ok_or("XBERG_PDF_TABLES_INVALID")?;
    let hay = literal_atoms(raw);
    let joined: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
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
                if literal_atoms(text).iter().any(|atom| {
                    !hay.contains(atom)
                        && !(atom.chars().all(char::is_alphabetic) && joined.contains(atom))
                        && !(atom.chars().all(|c| !c.is_alphanumeric()) && raw.contains(atom))
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
    fn cached_tables_are_bound_to_exact_revision_and_page_count() {
        let sha = "a".repeat(64);
        let value = json!({"source_sha256":sha,"page_count":2,"extractor":"xberg.1.3.10.pdf-tables.v1","tables":[]});
        assert!(validate_pdf(&value, &sha, 2).is_ok());
        assert!(validate_pdf(&value, &"b".repeat(64), 2).is_err());
        assert!(validate_pdf(&value, &sha, 1).is_err());
    }
}
