use super::{field, output, present, string, Result, INVALID, LIMIT, SCHEMA};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn number(value: &Value) -> Value {
    let text = string(value).replace(',', ".").replace([' ', '\u{a0}'], "");
    text.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map_or(Value::Null, |v| json!(v))
}
fn numeric(key: &str) -> bool {
    SCHEMA["numeric"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == key)
}
fn raw_pairs(value: &Value) -> Result<Vec<(String, Value)>> {
    let mut names = HashSet::new();
    value
        .as_array()
        .filter(|v| v.len() <= 512)
        .ok_or(INVALID)?
        .iter()
        .map(|pair| {
            let pair = pair.as_array().filter(|v| v.len() == 2).ok_or(INVALID)?;
            let key = pair[0]
                .as_str()
                .filter(|s| s.len() <= 65536)
                .ok_or(INVALID)?;
            if !names.insert(key) {
                return Err(INVALID);
            }
            Ok((key.into(), pair[1].clone()))
        })
        .collect()
}
fn raw_json(pairs: &[(String, Value)]) -> Result<String> {
    let parts = pairs
        .iter()
        .map(|(k, v)| {
            Ok(format!(
                "{}: {}",
                serde_json::to_string(k).map_err(|_| INVALID)?,
                serde_json::to_string(v).map_err(|_| INVALID)?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("{{{}}}", parts.join(", ")))
}
pub(super) fn rows(request: &Value) -> Result<Value> {
    let inputs = request["rows"]
        .as_array()
        .filter(|v| v.len() <= 20_000)
        .ok_or(INVALID)?;
    let mapping = request["mapping"]
        .as_object()
        .filter(|v| v.len() <= 512)
        .ok_or(INVALID)?;
    let reserved = [
        "doc_type",
        "doc_title",
        "source_file",
        "raw_row",
        "source_page",
        "table_index",
        "needs_ocr",
        "extractor",
    ];
    let mut out = Vec::new();
    for input in inputs {
        let pairs = raw_pairs(&input["raw"])?;
        let raw: Map<String, Value> = pairs.iter().cloned().collect();
        let mut norm = Map::new();
        for pair in SCHEMA["fields"].as_array().unwrap() {
            norm.insert(pair[0].as_str().unwrap().into(), pair[1].clone());
        }
        for key in ["doc_type", "doc_title", "source_file"] {
            norm.insert(key.into(), json!(field(request, key)));
        }
        norm.insert("raw_row".into(), json!(raw_json(&pairs)?));
        for key in ["source_page", "table_index", "needs_ocr", "extractor"] {
            if let Some(value) = raw.get(key) {
                norm.insert(key.into(), value.clone());
            }
        }
        for (column, target) in mapping {
            let target = target.as_str().ok_or(INVALID)?;
            if target == "skip" || !norm.contains_key(target) || reserved.contains(&target) {
                continue;
            }
            let Some(value) = raw.get(column).filter(|v| !v.is_null()) else {
                continue;
            };
            norm.insert(
                target.into(),
                if numeric(target) {
                    number(value)
                } else {
                    json!(string(value).trim())
                },
            );
        }
        if let Some(sheet) = input.get("sheet_name").filter(|v| v.is_string()) {
            norm.insert("sheet_name".into(), sheet.clone());
        }
        if let Some(row) = input
            .get("source_row")
            .filter(|v| v.as_u64().is_some_and(|n| n > 0))
        {
            norm.insert("source_row".into(), row.clone());
        }
        out.push(Value::Object(norm));
    }
    output(json!({"rows":out,"owner":"rust.tables.normalize"}))
}

/// Decode object keys in source order without enabling a second JSON map implementation.
fn ordered(text: &str) -> Vec<(String, Value)> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
    fn whitespace(bytes: &[u8], at: &mut usize) {
        while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
            *at += 1;
        }
    }
    whitespace(bytes, &mut at);
    if bytes.get(at) != Some(&b'{') {
        return out;
    }
    at += 1;
    loop {
        whitespace(bytes, &mut at);
        if bytes.get(at) == Some(&b'}') {
            return out;
        }
        let mut key = serde_json::Deserializer::from_slice(&bytes[at..]).into_iter::<String>();
        let Some(Ok(key_text)) = key.next() else {
            return Vec::new();
        };
        at += key.byte_offset();
        whitespace(bytes, &mut at);
        if bytes.get(at) != Some(&b':') {
            return Vec::new();
        }
        at += 1;
        let mut value = serde_json::Deserializer::from_slice(&bytes[at..]).into_iter::<Value>();
        let Some(Ok(value_text)) = value.next() else {
            return Vec::new();
        };
        at += value.byte_offset();
        out.push((key_text, value_text));
        if out.len() > 512 {
            return Vec::new();
        }
        whitespace(bytes, &mut at);
        match bytes.get(at) {
            Some(b',') => at += 1,
            Some(b'}') => return out,
            _ => return Vec::new(),
        }
    }
}
fn preferred<'a>(row: &'a Value, first: &str, second: &str) -> &'a Value {
    if present(&row[first]) {
        &row[first]
    } else {
        &row[second]
    }
}
fn chunk_text(row: &Value) -> String {
    let mut parts = Vec::new();
    let kind = field(row, "doc_type");
    let title = field(row, "doc_title");
    if !kind.is_empty() && !title.is_empty() {
        let label = SCHEMA["labels"][kind].as_str().unwrap_or(kind);
        parts.push(format!("Документ: {label} — {title}"));
    }
    if present(&row["section"]) {
        parts.push(format!("Раздел: {}", string(&row["section"])));
    }
    for (first, second, label) in [
        ("pos", "position", "Позиция"),
        ("name", "work_name", "Наименование"),
        ("code", "designation", "Обозначение/Артикул"),
    ] {
        let v = preferred(row, first, second);
        if present(v) {
            parts.push(format!("{label}: {}", string(v)));
        }
    }
    if present(&row["mark"]) {
        parts.push(format!("Марка/Тип: {}", string(&row["mark"])));
    }
    if present(&row["unit"]) && !row["qty"].is_null() {
        parts.push(format!(
            "Количество: {} {}",
            string(&row["qty"]),
            string(&row["unit"])
        ));
    }
    for (key, label) in [("price", "Цена"), ("amount", "Сумма")] {
        if let Some(v) = row[key].as_f64().filter(|v| v.is_finite()) {
            parts.push(format!("{label}: {v:.2}"));
        }
    }
    for (key, label) in [
        ("work_done", "Выполнено"),
        ("norms_refs", "Нормативы"),
        ("materials_used", "Материалы"),
        ("note", "Примечание"),
    ] {
        if present(&row[key]) {
            parts.push(format!("{label}: {}", string(&row[key])));
        }
    }
    if (parts.len() <= 1 || present(&row["sheet_name"])) && !field(row, "raw_row").is_empty() {
        let mut raw = Vec::new();
        let mut count = 0;
        for (key, value) in ordered(field(row, "raw_row")) {
            if value.is_null() {
                continue;
            }
            let v = string(&value);
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            let part = format!("{key}: {v}");
            count += part.chars().count() + if raw.is_empty() { 0 } else { 2 };
            raw.push(part);
            if count > 1200 {
                break;
            }
        }
        if !raw.is_empty() {
            parts.push(format!("Строка: {}", raw.join("; ")));
        }
    }
    if parts.is_empty() {
        row.to_string()
    } else {
        parts.join("\n")
    }
}
pub(super) fn chunks(request: &Value) -> Result<Value> {
    let rows = request["rows"]
        .as_array()
        .filter(|v| v.len() <= 20_000)
        .ok_or(INVALID)?;
    let mut out = Vec::new();
    for row in rows {
        if !row.is_object() {
            return Err(INVALID);
        }
        let text = chunk_text(row);
        if text.trim().is_empty() {
            continue;
        }
        if text.len() > 8 * 1024 * 1024 {
            return Err(LIMIT);
        }
        let mut metadata = Map::new();
        for key in ["doc_type", "doc_title", "source_file", "section", "unit"] {
            metadata.insert(key.into(), json!(field(row, key)));
        }
        for key in ["source_page", "table_index", "sheet_name", "qty", "amount"] {
            metadata.insert(key.into(), row[key].clone());
        }
        metadata.insert(
            "needs_ocr".into(),
            row.get("needs_ocr").cloned().unwrap_or(json!(false)),
        );
        metadata.insert("extractor".into(), json!(field(row, "extractor")));
        metadata.insert("row".into(), row["source_row"].clone());
        for (first, second, key) in [
            ("name", "work_name", "name"),
            ("code", "designation", "code"),
        ] {
            let value = preferred(row, first, second);
            metadata.insert(
                key.into(),
                if present(value) {
                    value.clone()
                } else {
                    json!("")
                },
            );
        }
        metadata.insert("type".into(), json!("table_row"));
        metadata.insert("dataset_id".into(), json!(field(request, "dataset_id")));
        out.push(json!({"text":text,"metadata":metadata,"raw":row}));
    }
    output(json!({"chunks":out,"owner":"rust.tables.chunks"}))
}
