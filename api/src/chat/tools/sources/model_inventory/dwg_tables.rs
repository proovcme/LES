use super::{envelope, source, ModelRows, Result, TOOL};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn lower(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
}

fn contains(value: &Value, query: &str) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.to_lowercase().contains(query))
}

pub(super) fn table(models: Vec<ModelRows>, args: &Value, revision: Value) -> Result<Value> {
    let query = lower(&args["q"]);
    let title = lower(&args["table"]);
    let sheet = lower(&args["sheet"]);
    let layer = lower(&args["layer"]);
    let mut matches = Vec::new();
    for model in &models {
        for row in &model.records {
            let p = &row["payload"];
            let saved = &p["indexed_engineering_record"];
            let cells = saved["cells"].as_array().ok_or("INDEX_READ_INVALID")?;
            let handle = saved["table_handle"].as_str().ok_or("INDEX_READ_INVALID")?;
            let row_number = saved["row"].as_u64().ok_or("INDEX_READ_INVALID")?;
            let layout = &p["model_locator"]["drawing_sheet"]["name"];
            if title
                .as_ref()
                .is_some_and(|q| !contains(&saved["title"], q) && !handle.eq_ignore_ascii_case(q))
                || sheet
                    .as_ref()
                    .is_some_and(|q| lower(layout).as_ref() != Some(q))
                || layer.as_ref().is_some_and(|q| {
                    !["layers", "grid_layers"].iter().any(|key| {
                        saved[*key]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|v| lower(v).as_ref() == Some(q))
                    })
                })
                || query.as_ref().is_some_and(|q| {
                    !cells.iter().any(|cell| contains(&cell["text"], q))
                        && !contains(&saved["title"], q)
                })
            {
                continue;
            }
            let order = saved["table_order"].as_u64().unwrap_or(0);
            matches.push((
                &model.doc, row, saved, cells, layout, handle, row_number, order,
            ));
        }
    }
    matches.sort_by(|a, b| {
        a.0["file_name"]
            .as_str()
            .cmp(&b.0["file_name"].as_str())
            .then(a.0["id"].as_str().cmp(&b.0["id"].as_str()))
            .then(a.4.as_str().cmp(&b.4.as_str()))
            .then(a.7.cmp(&b.7))
            .then(a.5.cmp(b.5))
            .then(a.6.cmp(&b.6))
    });
    let total_rows = matches.len();
    let data_rows = matches.iter().filter(|m| m.2["kind"] == "data").count();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(64).clamp(1, 64) as usize;
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    let mut tables = Vec::new();
    let mut table_ids = BTreeSet::new();
    let mut budget = 0;
    for (doc, point, saved, cells, layout, handle, number, _) in
        matches.into_iter().skip(offset).take(limit)
    {
        let values = cells
            .iter()
            .map(|cell| cell["text"].as_str().unwrap_or(""))
            .collect::<Vec<_>>();
        let size = values.iter().map(|v| v.len()).sum::<usize>() + 300;
        if !rows.is_empty() && budget + size > 26_000 {
            break;
        }
        budget += size;
        let index = sources.len() + 1;
        let mut evidence = source(doc, point);
        evidence["index"] = json!(index);
        let formatted = cells
            .iter()
            .zip(&values)
            .filter(|(_, value)| !value.is_empty())
            .map(|(cell, value)| {
                cell["heading"]
                    .as_str()
                    .map_or_else(|| value.to_string(), |h| format!("{h}: {value}"))
            })
            .collect::<Vec<_>>()
            .join(" | ");
        evidence["snippet"] = json!(formatted);
        let doc_id = doc["id"].as_str().ok_or("INDEX_READ_INVALID")?;
        if table_ids.insert((doc_id, handle)) {
            tables.push(json!({"doc_id":doc_id,"file_name":doc["file_name"],"handle":handle,
                "title":saved["title"],"sheet":layout,"method":saved["method"],"layers":saved["layers"],
                "grid_layers":saved["grid_layers"],
                "columns":cells.iter().map(|cell| &cell["heading"]).collect::<Vec<_>>()}));
        }
        rows.push(
            json!({"doc_id":doc_id,"table_handle":handle,"row":number,"values":values,
            "kind":saved["kind"],"source_index":index}),
        );
        sources.push(evidence);
    }
    let next = (offset + rows.len() < total_rows).then_some(offset + rows.len());
    let model_coverage = models.iter().map(|m| json!({
        "doc_id":m.doc["id"],"file_name":m.doc["file_name"],"structured_rows":m.records.len(),
        "status":if m.records.is_empty() {"records_unavailable"} else {"read"}
    })).collect::<Vec<_>>();
    Ok(envelope(
        TOOL,
        "dwg_tables",
        json!({"schema":"les.model-inventory.v1","mode":"dwg_tables",
        "rows":rows,"tables":tables,"offset":offset,"next_offset":next,"total_rows":total_rows,
        "data_rows":data_rows,"index_revision":revision,"coverage":"indexed_dwg_tables",
        "complete_model":false,"models":model_coverage}),
        sources,
    ))
}
