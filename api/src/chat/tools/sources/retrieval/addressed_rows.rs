//! Fetch explicit spreadsheet addresses inside the same scope and revision as search.
use super::{original_reader, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub(super) async fn read(
    context: &Context,
    store: &crate::index_store::Store,
    collection: &str,
    datasets: &[Value],
    files: &[Value],
    addresses: &BTreeMap<&str, BTreeSet<u64>>,
) -> Result<Vec<Value>> {
    let mut selected = Vec::new();
    for (&sheet, rows) in addresses {
        let mut must = vec![
            json!({"key":"dataset_id","match":{"any":datasets}}),
            json!({"key":"sheet_name","match":{"value":sheet}}),
            json!({"key":"row","match":{"any":rows}}),
        ];
        if !files.is_empty() {
            must.push(json!({"key":"file_name","match":{"any":files}}));
        }
        let result = store
            .read_owned(
                context,
                collection,
                "scroll",
                json!({"limit":128,"with_payload":true,"with_vector":false,
                    "filter":{"must":must,"must_not":[{"key":"node_role","match":{"value":"navigation"}},
                        {"key":"evidence_admissible","match":{"value":false}}]}}),
            )
            .await?;
        // Do not choose a workbook or a conflicting reading from an
        // incomplete list of matches.
        if !result["next_page_offset"].is_null() {
            continue;
        }
        let points = result["points"].as_array().ok_or("INDEX_READ_INVALID")?;
        let mut owners = BTreeSet::new();
        let mut matches = BTreeMap::new();
        for point in points {
            let p = &point["payload"];
            let row = p["row"].as_u64().ok_or("INDEX_READ_INVALID")?;
            if !datasets.contains(&p["dataset_id"])
                || (!files.is_empty() && !files.contains(&p["file_name"]))
                || p["sheet_name"] != sheet
                || !rows.contains(&row)
                || p["node_role"] == "navigation"
                || p["evidence_admissible"] == false
            {
                return Err("TOOL_SCOPE_VIOLATION".into());
            }
            let file = p["file_name"].as_str().ok_or("INDEX_READ_INVALID")?;
            if original_reader(file) != "read_excel_source" {
                continue;
            }
            owners.insert((p["dataset_id"].as_str(), file, p["source_sha256"].as_str()));
            if p["source_sha256"]
                .as_str()
                .is_none_or(|sha| sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()))
                || p["text"].as_str().is_none_or(|text| text.trim().is_empty())
                || p["source_read_required"] == true
            {
                continue;
            }
            matches
                .entry(row)
                .and_modify(|v| *v = None)
                .or_insert(Some(point));
        }
        if owners.len() == 1 {
            selected.extend(matches.into_values().flatten().cloned());
        }
    }
    Ok(selected)
}

fn book(point: &Value) -> [Value; 3] {
    ["dataset_id", "file_name", "source_sha256"].map(|k| point["payload"][k].clone())
}

fn coordinate(point: &Value) -> ([Value; 3], Value, Value) {
    let p = &point["payload"];
    (book(point), p["sheet_name"].clone(), p["row"].clone())
}

pub(super) fn prioritize(mut addressed: Vec<Value>, ranked: Vec<Value>) -> Vec<Value> {
    if addressed.is_empty() {
        return ranked;
    }
    let books: HashSet<_> = addressed.iter().map(book).collect();
    let coordinates: HashSet<_> = addressed.iter().map(coordinate).collect();
    addressed.extend(
        ranked
            .into_iter()
            .filter(|r| !coordinates.contains(&coordinate(r))),
    );
    for point in &mut addressed {
        if books.contains(&book(point)) {
            // Keep selected rows individually addressable at the start of
            // the window even when the whole book is available.
            point["prefer_fragments"] = json!(true);
        }
    }
    addressed
}
