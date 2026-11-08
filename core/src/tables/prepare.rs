//! Source-sheet normalization and row construction share one owner.
use super::{field, normalize, output, Result, INVALID, LIMIT, SCHEMA};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn mapping(headers: &[String]) -> Map<String, Value> {
    headers
        .iter()
        .map(|header| {
            let lower = header.to_lowercase();
            let key = SCHEMA["mapping_rules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|rule| {
                    rule[1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|hint| lower.contains(hint.as_str().unwrap()))
                })
                .map_or("skip", |rule| rule[0].as_str().unwrap());
            (header.clone(), json!(key))
        })
        .collect()
}
fn kind(headers: &[String], sheet: &str) -> String {
    let text = format!("{} {sheet}", headers.join(" ")).to_lowercase();
    if ["позиция", "поз.", "поз "].iter().any(|v| text.contains(v))
        && ["кол-во", "количество", "ед.изм", "единица"]
            .iter()
            .any(|v| text.contains(v))
    {
        return "SPEC".into();
    }
    let mut selected = "TABLE";
    let mut score = 0;
    for key in SCHEMA["kind_priority"].as_array().unwrap() {
        let key = key.as_str().unwrap();
        let hints = &SCHEMA["kind_hints"][key];
        let next = hints
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| text.contains(v.as_str().unwrap()))
            .count();
        if next > score {
            score = next;
            selected = key;
        }
    }
    selected.into()
}
pub(crate) fn prepare(request: &Value) -> Result<Value> {
    let sheets = request["sheets"]
        .as_array()
        .filter(|v| v.len() <= 512)
        .ok_or(INVALID)?;
    let mut all = Vec::new();
    let mut last = "TABLE".to_owned();
    for sheet in sheets {
        let headers = sheet["headers"]
            .as_array()
            .filter(|v| v.len() <= 512)
            .ok_or(INVALID)?;
        let mut seen = HashMap::<String, usize>::new();
        let mut unique = Vec::new();
        for (index, header) in headers.iter().enumerate() {
            let text = header.as_str().ok_or(INVALID)?.trim();
            let key = if text.is_empty() {
                format!("col_{}", index + 1)
            } else {
                text.into()
            };
            let count = seen.entry(key.clone()).or_default();
            *count += 1;
            unique.push(if *count == 1 {
                key
            } else {
                format!("{key}_{}", *count)
            });
        }
        let rows = sheet["rows"]
            .as_array()
            .filter(|v| v.len() <= 20_000)
            .ok_or(INVALID)?;
        let name = field(sheet, "sheet_name");
        let override_kind = field(request, "doc_type");
        last = if SCHEMA["labels"][override_kind].is_string() {
            override_kind.into()
        } else {
            kind(&unique, name)
        };
        let mapped = mapping(&unique);
        let mut inputs = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let row = row
                .as_array()
                .filter(|v| v.len() == unique.len())
                .ok_or(INVALID)?;
            let mut raw = unique
                .iter()
                .zip(row)
                .map(|(k, v)| json!([k, v]))
                .collect::<Vec<_>>();
            for key in ["source_page", "table_index", "needs_ocr", "extractor"] {
                if let Some(v) = sheet.get(key) {
                    raw.push(json!([key, v]));
                }
            }
            let mut input = json!({"raw":raw});
            if request["workbook"] == true {
                input["sheet_name"] = json!(name);
                input["source_row"] = sheet["row_numbers"][index].clone();
            }
            inputs.push(input);
        }
        let config = json!({"rows":inputs,"mapping":mapped,"doc_type":last,
            "doc_title":format!("{} / {name}",field(request,"stem")),"source_file":request["source_file"]});
        let normalized = normalize::rows(&config)?;
        all.extend(
            normalized["rows"]
                .as_array()
                .ok_or(INVALID)?
                .iter()
                .cloned(),
        );
        if all.len() > 200_000 {
            return Err(LIMIT);
        }
    }
    let chunks = normalize::chunks(&json!({"rows":all,"dataset_id":request["dataset_id"]}))?;
    output(
        json!({"rows":all,"chunks":chunks["chunks"],"doc_type":last,"sheets":sheets.len(),"owner":"rust.tables.prepare"}),
    )
}
