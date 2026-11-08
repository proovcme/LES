use super::*;
pub const CELL_POLICY:&str="Coordinates are literal. Absent coordinates are blank. Numbers are decimal strings; formula caches may be absent. All nonempty rows, including headings, are included.";
pub fn snapshot(path: &Path, name: &str) -> Result<Value> {
    let mut table = if path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("csv"))
    {
        crate::document::conversion::csv_snapshot(path)?
    } else {
        crate::document::workbook::snapshot(&json!({"path":path}))?
    };
    table["name"] = json!(name);
    table["schema"] = json!("les.table.v1");
    table["cell_policy"] = json!(CELL_POLICY);
    table["row_count"] = json!(table["rows"]
        .as_array()
        .ok_or("ATTACHMENT_TABLE_INVALID")?
        .len());
    Ok(table)
}
pub(super) fn table_preview(table: &Value, limit: usize) -> Result<(String, bool)> {
    let rows = table["rows"].as_array().ok_or("ATTACHMENT_TABLE_INVALID")?;
    let mut parts = vec![
        format!("Файл: {}", table["name"].as_str().unwrap_or("")),
        CELL_POLICY.into(),
        format!("Всего непустых строк: {}", rows.len()),
    ];
    let mut used = parts.join("\n").chars().count();
    let mut shown = 0;
    for row in rows {
        let line = format!(
            "{}: {}",
            row["locator"].as_str().ok_or("ATTACHMENT_TABLE_INVALID")?,
            crate::tool_execution::canonical::encode(&row["cells"])?
        );
        if used + line.chars().count() + 160 > limit {
            break;
        }
        used += line.chars().count() + 1;
        parts.push(line);
        shown += 1;
    }
    let truncated = shown < rows.len();
    if truncated {
        parts.push(format!(
            "Показано {shown} из {} строк. Полное чтение — инструментом table_document.",
            rows.len()
        ));
    }
    Ok((parts.join("\n"), truncated))
}
pub fn execute(req: &Value) -> Result<Value> {
    let path = Path::new(req["path"].as_str().ok_or("ATTACHMENT_PATH_UNSAFE")?);
    let name = req["name"]
        .as_str()
        .unwrap_or_else(|| path.file_name().and_then(|s| s.to_str()).unwrap_or("file"));
    let limit = req["max_chars"].as_u64().unwrap_or(18000).min(20000) as usize;
    let suffix = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(suffix.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        let mut result = picture::file(path)?;
        result["text"] = json!("");
        result["truncated"] = json!(false);
        result["media_type"] = json!("image");
        return Ok(result);
    }
    if matches!(suffix.as_str(), "xlsx" | "xlsm" | "csv") {
        let value = snapshot(path, name)?;
        let (text, truncated) = table_preview(&value, limit)?;
        return Ok(
            json!({"text":text,"truncated":truncated,"chars":text.chars().count(),"media_type":"document","source_sha256":value["sha256"]}),
        );
    }
    Err("ATTACHMENT_NATIVE_CONVERTER_REQUIRED")
}
/// The API supplies its native PDF/OCR callback in the same killable worker.
pub fn convert(req: &Value, external: &impl Fn(&Value) -> Result<Value>) -> Result<Value> {
    match execute(req) {
        Err("ATTACHMENT_NATIVE_CONVERTER_REQUIRED") => {}
        result => {
            let value = result?;
            if let Some(sha) = req.get("source_sha256") {
                if value["source_sha256"].is_string() && value["source_sha256"] != *sha {
                    return Err("ATTACHMENT_HASH_CHANGED");
                }
            }
            return Ok(value);
        }
    }
    let mut input = json!({"schema":"les.document-convert.v1","path":req["path"]});
    if let Some(state) = req.get("owned_state") {
        input["owned_state"] = state.clone();
    }
    if let Some(sha) = req.get("source_sha256") {
        input["source_sha256"] = sha.clone();
    }
    let value = crate::document::conversion::execute_with(&input, external)?;
    let text = value["text"]
        .as_str()
        .ok_or("ATTACHMENT_READ_FAILED")?
        .trim();
    if text.is_empty() {
        return Err("ATTACHMENT_NO_TEXT");
    }
    let max = req["max_chars"].as_u64().unwrap_or(18000).min(20000) as usize;
    Ok(
        json!({"text":text.chars().take(max).collect::<String>(),"chars":text.chars().count(),"truncated":text.chars().count()>max,"media_type":"document","source_sha256":value["source_sha256"]}),
    )
}
