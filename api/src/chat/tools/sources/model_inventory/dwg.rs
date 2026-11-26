//! A DWG quantity is an insertion count from Model space, with power only from saved attributes.
use super::{envelope, source, ModelRows, Result, TOOL};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn attribute<'a>(attributes: &'a [Value], tag: &str) -> Option<&'a str> {
    let tag = tag.to_uppercase();
    let values: BTreeSet<_> = attributes
        .iter()
        .filter(|attr| text(&attr["tag"]).is_some_and(|s| s.to_uppercase() == tag))
        .filter_map(|attr| text(&attr["value"]))
        .collect();
    (values.len() == 1).then(|| *values.first().unwrap())
}

fn watts(attributes: &[Value]) -> Option<f64> {
    let value = attribute(attributes, "POWER_W")?.to_lowercase();
    let (value, factor) = [("квт", 1000.0), ("kw", 1000.0), ("вт", 1.0), ("w", 1.0)]
        .into_iter()
        .find_map(|(unit, factor)| value.strip_suffix(unit).map(|number| (number, factor)))
        .unwrap_or((&value, 1.0));
    let amount = value.trim().replace(',', ".").parse::<f64>().ok()? * factor;
    (amount.is_finite() && amount >= 0.0).then_some(amount)
}

fn lighting_label(value: &str) -> bool {
    let label = value.to_lowercase();
    let mut words = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty());
    words.clone().any(|word| {
        matches!(word, "light" | "lighting" | "luminaire")
            || word.starts_with("светильник")
            || word.starts_with("освещен")
    }) || (words.clone().any(|word| matches!(word, "eo" | "эо"))
        && words.any(|word| matches!(word, "emerg" | "emergency" | "аварийное")))
}

struct Group<'a> {
    doc: &'a Value,
    example: &'a Value,
    quantity: u64,
    powered: u64,
    watts: f64,
}

pub(super) fn table(models: Vec<ModelRows>, args: &Value, revision: Value) -> Result<Value> {
    let lighting_only = args["lighting_only"] == true;
    let filter = text(&args["q"]).map(str::to_lowercase);
    let mut groups = BTreeMap::new();
    let mut count = 0_u64;
    let mut powered = 0_u64;
    let mut total_w = 0.0;
    for model in &models {
        for row in &model.records {
            let saved = &row["payload"]["indexed_engineering_record"];
            let name = text(&saved["name"]).ok_or("INDEX_READ_INVALID")?;
            let layer = text(&saved["layer"]);
            let quantity = saved["quantity"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or("INDEX_READ_INVALID")?;
            if saved["space"] != "model" || text(&saved["owner_handle"]).is_none() {
                return Err("INDEX_READ_INVALID".into());
            }
            if text(&args["block"]).is_some_and(|s| s.to_lowercase() != name.to_lowercase())
                || text(&args["layer"])
                    .is_some_and(|s| !layer.is_some_and(|v| v.to_lowercase() == s.to_lowercase()))
                || (lighting_only && !lighting_label(name) && !layer.is_some_and(lighting_label))
            {
                continue;
            }
            let attrs = saved["attributes"].as_array().ok_or("INDEX_READ_INVALID")?;
            let mark = ["MARK", "МАРКА", "TYPE", "ТИП"]
                .into_iter()
                .find_map(|tag| attribute(attrs, tag));
            if filter.as_ref().is_some_and(|q| {
                ![Some(name), mark, layer]
                    .into_iter()
                    .flatten()
                    .any(|v| v.to_lowercase().contains(q))
            }) {
                continue;
            }
            let key = (
                model.doc["id"].as_str().ok_or("INDEX_READ_INVALID")?,
                name,
                mark,
                layer,
            );
            let group = groups.entry(key).or_insert(Group {
                doc: &model.doc,
                example: row,
                quantity: 0,
                powered: 0,
                watts: 0.0,
            });
            group.quantity = group
                .quantity
                .checked_add(quantity)
                .ok_or("MODEL_INVENTORY_LIMIT")?;
            count = count.checked_add(quantity).ok_or("MODEL_INVENTORY_LIMIT")?;
            if let Some(unit_w) = watts(attrs) {
                let amount = unit_w * quantity as f64;
                group.powered += quantity;
                powered += quantity;
                group.watts += amount;
                total_w += amount;
                if !total_w.is_finite() {
                    return Err("MODEL_INVENTORY_LIMIT".into());
                }
            }
        }
    }
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 24) as usize;
    let total_rows = groups.len();
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    for ((doc_id, name, mark, layer), group) in groups.into_iter().skip(offset).take(limit) {
        let index = sources.len() + 1;
        let mut evidence = source(group.doc, group.example);
        evidence["index"] = json!(index);
        rows.push(json!({"doc_id":doc_id,"name":name,"mark":mark,"layer":layer,"quantity":group.quantity,
            "power_w":(group.powered > 0).then_some(group.watts),"power_known":group.powered,"power_unknown":group.quantity - group.powered,
            "example_handle":evidence["model_locator"]["native_id"],"source_index":index}));
        sources.push(evidence);
    }
    let model_coverage: Vec<_> = models.iter().map(|model| json!({
        "doc_id":model.doc["id"],"doc_name":model.doc["file_name"],"source_sha256":model.doc["file_hash"],
        "status":if model.doc["status"] != "INDEXED" { "not_indexed" } else if model.records.is_empty() {"records_unavailable"} else {"read"},
        "structured_records":model.records.len(),
        "reader_coverage":model.records.first().map(|r| &r["payload"]["model_locator"]["coverage"])
    })).collect();
    let next = (offset + rows.len() < total_rows).then_some(offset + rows.len());
    Ok(envelope(
        TOOL,
        "dwg_blocks",
        json!({"schema":"les.model-inventory.v1","mode":"dwg_blocks",
        "q":args["q"],"block":args["block"],"layer":args["layer"],"lighting_only":lighting_only,
        "rows":rows,"offset":offset,"next_offset":next,"total_rows":total_rows,"matched_instances":count,
        "power":{"sum_w":(powered > 0).then_some(total_w),"known":powered,"unknown":count - powered,"attribute":"POWER_W"},
        "index_revision":revision,"coverage":"indexed_model_space_inserts","complete_model":false,"models":model_coverage}),
        sources,
    ))
}
