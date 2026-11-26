//! Follow saved instance-to-type references inside a frozen index generation.
use super::{packing, Context, Result};
use crate::index_store::Store;
use les_core::{
    engineering::{digest, record_key, rvt_parameter_line, RECORD_SCHEMA},
    lexical,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

const MAX_ANCHORS: usize = 8;
const MAX_RECORDS: usize = 16;
const MAX_CHARS: usize = 1200;
const CHANGED: &str = "INDEX_CHANGED_DURING_SEARCH";

struct Target<'a> {
    dataset: &'a str,
    file: &'a str,
    sha: &'a str,
    section: &'a str,
    native_id: &'a str,
    name: Option<&'a str>,
    rank: u64,
}

fn targets(points: &[Value]) -> BTreeMap<String, Target<'_>> {
    let mut result = BTreeMap::new();
    let mut anchors = HashSet::new();
    for (index, point) in points.iter().enumerate() {
        let p = &point["payload"];
        let locator = &p["model_locator"];
        if locator["provider"] != "les"
            || locator["reader"]["format"] != "rvt"
            || locator["record_schema"] != RECORD_SCHEMA
        {
            continue;
        }
        let (Some(dataset), Some(file), Ok(sha), Some(section), Some(native_id)) = (
            p["dataset_id"].as_str(),
            p["file_name"].as_str(),
            digest(&p["source_sha256"]),
            locator["section"].as_str(),
            locator["native_id"].as_str(),
        ) else {
            continue;
        };
        if locator["source_sha256"] != sha
            || !anchors.insert((dataset, file, sha, section, native_id))
        {
            continue;
        }
        if anchors.len() > MAX_ANCHORS {
            break;
        }
        let links = locator["model_context"]["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|link| Some(("types", link["native_id"].as_str()?, link["name"].as_str())));
        for (section, native_id, name) in links.chain(std::iter::once((section, native_id, None))) {
            if result.len() >= MAX_RECORDS {
                break;
            }
            if !native_id.parse::<u64>().is_ok_and(|id| id > 0) {
                continue;
            }
            result
                .entry(record_key(dataset, file, sha, section, native_id))
                .or_insert(Target {
                    dataset,
                    file,
                    sha,
                    section,
                    native_id,
                    name,
                    rank: packing::fused_rank(point, index),
                });
        }
    }
    result
}

fn matches(p: &Value, target: &Target<'_>) -> bool {
    let locator = &p["model_locator"];
    p["dataset_id"] == target.dataset
        && p["file_name"] == target.file
        && p["source_sha256"] == target.sha
        && locator["source_sha256"] == target.sha
        && locator["provider"] == "les"
        && locator["reader"]["format"] == "rvt"
        && locator["record_schema"] == RECORD_SCHEMA
        && locator["section"] == target.section
        && locator["native_id"] == target.native_id
        && p["node_role"] == "evidence"
        && p["evidence_admissible"] != false
        && p["indexed_engineering_record"]["schema"] == RECORD_SCHEMA
}

fn project(
    row: &Value,
    target: &Target<'_>,
    query_terms: &serde_json::Map<String, Value>,
) -> Result<Option<Value>> {
    let p = &row["payload"];
    let record = &p["indexed_engineering_record"];
    let parameters = record["parameters"].as_array().ok_or(CHANGED)?;
    let mut matching = Vec::new();
    for batch in parameters.chunks(128) {
        let labels = batch
            .iter()
            .map(|param| {
                param["name"]
                    .as_str()
                    .unwrap_or("")
                    .replace('_', " ")
                    .chars()
                    .take(240)
                    .collect()
            })
            .collect::<Vec<_>>();
        let normalized = lexical::reading(&labels)?;
        for (param, terms) in batch
            .iter()
            .zip(normalized["documents"].as_array().ok_or(CHANGED)?)
        {
            if param["value"].is_null() {
                continue;
            }
            let matches = terms["terms"]
                .as_object()
                .ok_or(CHANGED)?
                .keys()
                .filter(|word| query_terms.contains_key(*word))
                .count();
            if matches > 0 {
                if let Some(line) = rvt_parameter_line(param) {
                    matching.push((matches, line));
                }
            }
        }
    }
    matching.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    let label = if target.section == "types" {
        "Параметры типа Revit"
    } else {
        "Параметры объекта Revit"
    };
    let name = target
        .name
        .or_else(|| record["name"].as_str())
        .unwrap_or("");
    let mut text = format!("{label} {name} (ElementId {})", target.native_id);
    let mut chars = text.chars().count();
    let mut included = 0;
    let mut seen = HashSet::new();
    for (_, line) in matching {
        let length = 1 + line.chars().count();
        if chars + length > MAX_CHARS || !seen.insert(line.clone()) {
            continue;
        }
        text.push('\n');
        text.push_str(&line);
        chars += length;
        included += 1;
    }
    if included == 0 {
        return Ok(None);
    }
    let id = row["id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| row["id"].as_u64().map(|n| n.to_string()))
        .ok_or(CHANGED)?;
    let mut payload = p.clone();
    payload["text"] = json!(text);
    payload["reading_representation"] = json!("indexed_rvt_record");
    payload["retrieval_anchor_id"] = row["id"].clone();
    payload["retrieval_source_rank"] = json!(target.rank);
    payload
        .as_object_mut()
        .unwrap()
        .remove("indexed_engineering_record");
    Ok(Some(
        json!({"id":format!("{id}:rvt-record"),"payload":payload}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_parameter_from_same_revision_enters_the_question_context() {
        let sha = "a".repeat(64);
        let instance = json!({"id":"instance-point","payload":{
            "dataset_id":"ds","file_name":"MEP.rvt","source_sha256":sha,"parent_id":"instance",
            "text":"Циркуляционный насос Н1 на первом этаже.",
            "model_locator":{"provider":"les","reader":{"format":"rvt"},"record_schema":RECORD_SCHEMA,
                "source_sha256":sha,"section":"elements","native_id":"51",
                "model_context":{"types":[{"native_id":"97","name":"Насос 40-120"}]}}
        }});
        let targets = targets(std::slice::from_ref(&instance));
        assert_eq!(targets.len(), 2);
        let key = record_key("ds", "MEP.rvt", &sha, "types", "97");
        let target = &targets[&key];
        let row = json!({"id":"type-point","payload":{
            "dataset_id":"ds","file_name":"MEP.rvt","source_sha256":sha,"parent_id":"type",
            "engineering_record_key":key,"node_role":"evidence",
            "model_locator":{"provider":"les","reader":{"format":"rvt"},"record_schema":RECORD_SCHEMA,
                "source_sha256":sha,"section":"types","native_id":"97"},
            "indexed_engineering_record":{"schema":RECORD_SCHEMA,"parameters":[
                {"name":"ADSK_Номинальный расход","value":2.75,"unit":"л/с"},
                {"name":"Длина","value":200,"unit":"мм"},
                {"name":"Номинальный расход","value":null},
                {"parameter_id":431,"value":42}
            ]}
        }});
        assert!(matches(&row["payload"], target));
        let mut other_revision = row["payload"].clone();
        other_revision["source_sha256"] = json!("b".repeat(64));
        assert!(!matches(&other_revision, target));
        let mut other_model = row["payload"].clone();
        other_model["file_name"] = json!("Another.rvt");
        assert!(!matches(&other_model, target));
        let query = "Какой номинальный расход у насоса Н1?";
        let terms = lexical::reading(&[query.to_owned()]).unwrap();
        let point = project(
            &row,
            target,
            terms["documents"][0]["terms"].as_object().unwrap(),
        )
        .unwrap()
        .unwrap();
        let text = point["payload"]["text"].as_str().unwrap();
        assert!(text.contains("ADSK_Номинальный расход [л/с]: 2.75"));
        assert!(!text.contains("Длина") && !text.contains("null") && !text.contains("431"));
        assert_eq!(point["payload"]["retrieval_anchor_id"], "type-point");
        assert_eq!(point["payload"]["model_locator"]["native_id"], "97");
        let (selected, _) = packing::controlled_query(
            &[instance],
            &json!([{"points":[point]}]),
            &Value::Null,
            2000,
            query,
            super::super::trial::Control::Off,
            true,
        )
        .unwrap();
        assert!(selected
            .iter()
            .any(|p| p["payload"]["retrieval_anchor_id"] == "type-point"));
    }
}

pub(super) async fn read(
    context: &Context,
    store: &Store,
    collection: &str,
    revision: &Value,
    points: &[Value],
    query: &str,
) -> Result<Vec<Value>> {
    let targets = targets(points);
    if targets.is_empty() || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let normalized = lexical::reading(&[query.to_owned()])?;
    let terms = normalized["documents"][0]["terms"]
        .as_object()
        .ok_or(CHANGED)?;
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    if store.revision(context, collection).await? != *revision {
        return Err(CHANGED.into());
    }
    let reply = store.read_owned(context, collection, "scroll", json!({
        "filter":{"must":[{"key":"engineering_record_key","match":{"any":targets.keys().collect::<Vec<_>>()}}]},
        "limit":MAX_RECORDS+1,"with_payload":true,"with_vector":false
    })).await?;
    let rows = reply["points"].as_array().ok_or(CHANGED)?;
    if !reply["next_page_offset"].is_null() || rows.len() > targets.len() {
        return Err(CHANGED.into());
    }
    let mut seen = HashSet::new();
    let mut related = Vec::new();
    for row in rows {
        let p = &row["payload"];
        let key = p["engineering_record_key"].as_str().ok_or(CHANGED)?;
        let target = targets.get(key).ok_or(CHANGED)?;
        if !seen.insert(key) || !matches(p, target) {
            return Err(CHANGED.into());
        }
        if let Some(evidence) = project(row, target, terms)? {
            related.push(evidence);
        }
    }
    if store.revision(context, collection).await? != *revision {
        return Err(CHANGED.into());
    }
    Ok(related)
}
