//! Count saved RVT instances by type and level using the local reader's records.
use super::{envelope, reference, Context, Result, TOOL};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

fn snapshot(context: &Context, doc: &Value, args: &Value) -> Result<(Value, String)> {
    let saved = les_core::engineering::rvt_inventory::load(&context.database, doc)?
        .ok_or("RVT_SNAPSHOT_NOT_READY")?;
    let key = les_core::sha256_hex(
        json!([
            saved["schema"],
            doc["file_hash"],
            saved["reader"]["revision"]
        ])
        .to_string()
        .as_bytes(),
    );
    let revision = format!("rvt:{key}");
    if args["index_revision"].is_string() && args["index_revision"] != revision {
        return Err("INDEX_CHANGED_DURING_SEARCH".into());
    }
    Ok((saved, revision))
}

fn link<'a>(record: &'a Value, field: &str) -> Option<&'a Value> {
    let items = record["model_context"][field].as_array()?;
    if let [one] = items.as_slice() {
        Some(one)
    } else {
        None
    }
}

fn matches_literal(value: &Value, query: &str) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.to_lowercase().contains(query))
}

#[derive(Default)]
struct Group<'a> {
    members: BTreeMap<&'a str, &'a Value>,
    type_name: Option<&'a str>,
    family: Option<&'a str>,
    level_name: Option<&'a str>,
}

fn table(doc: Value, saved: &Value, args: &Value, revision: &str) -> Result<Value> {
    let records = saved["objects"]
        .as_array()
        .ok_or("ENGINEERING_READER_FAILED")?;
    let types = records
        .iter()
        .filter(|r| r["section"] == "types")
        .filter_map(|r| Some((r["native_id"].as_str()?, r)))
        .collect::<BTreeMap<_, _>>();
    let query = args["q"].as_str().map(|s| s.trim().to_lowercase());
    let level_filter = args["level_id"].as_str();
    let type_filter = args["type_id"].as_str();
    let category_filter = args["category_id"].as_i64();
    let mut groups = BTreeMap::new();
    let mut unresolved_type = 0;
    for instance in records.iter().filter(|r| r["section"] == "elements") {
        let id = instance["native_id"]
            .as_str()
            .ok_or("SOURCE_IDENTITY_INVALID")?;
        let Some(typ) = link(instance, "types") else {
            unresolved_type += 1;
            continue;
        };
        let type_id = typ["native_id"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?;
        let type_record = types.get(type_id).copied().unwrap_or(&Value::Null);
        let family = link(type_record, "families").unwrap_or(&Value::Null);
        let level = link(instance, "levels").unwrap_or(&Value::Null);
        let level_id = level["native_id"].as_str();
        let category = instance["category_id"].as_i64();
        if type_filter.is_some_and(|value| type_id != value)
            || level_filter.is_some_and(|value| level_id != Some(value))
            || category_filter.is_some_and(|value| category != Some(value))
            || query.as_ref().is_some_and(|query| {
                ![
                    &instance["name"],
                    &typ["name"],
                    &type_record["name"],
                    &family["name"],
                ]
                .into_iter()
                .any(|value| matches_literal(value, query))
            })
        {
            continue;
        }
        let group = groups
            .entry((type_id, level_id, category))
            .or_insert_with(|| Group {
                type_name: typ["name"]
                    .as_str()
                    .or_else(|| type_record["name"].as_str()),
                family: family["name"].as_str(),
                level_name: level["name"].as_str(),
                members: BTreeMap::new(),
            });
        if group.members.insert(id, instance).is_some() {
            return Err("SOURCE_IDENTITY_INVALID".into());
        }
    }
    let total = groups.len();
    let matched = groups
        .values()
        .map(|group| group.members.len())
        .sum::<usize>();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(24).clamp(1, 64) as usize;
    let origin = json!({"origin":{"provider":"les","source":{"filename":doc["file_name"],"sha256":doc["file_hash"]},
        "coverage":saved["coverage"],"reader":saved["reader"]}});
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    let mut ids_in_page = 0;
    for ((type_id, level_id, category), group) in groups.into_iter().skip(offset).take(limit) {
        // Keep response bounds while preserving the exact count and a stable continuation.
        if !rows.is_empty() && ids_in_page + group.members.len() > 4_096 {
            break;
        }
        let (first, instance) = group
            .members
            .first_key_value()
            .ok_or("SOURCE_IDENTITY_INVALID")?;
        let count = group.members.len();
        let mut source = reference(&doc, Value::Null);
        let index = sources.len() + 1;
        source["index"] = json!(index);
        source["model_locator"] = les_core::engineering::locator(&origin, instance);
        source["evidence_kind"] = json!("engineering_snapshot");
        source["evidence_admissible"] = json!(true);
        source["reading_representation"] = json!("saved_rvt_counts");
        source["snippet"] = json!(format!("Тип {} (ElementId {type_id}). Уровень {}. Экземпляров по прочитанным записям: {count}. Пример: ElementId {first}.",
            group.type_name.unwrap_or(type_id), group.level_name.or(level_id).unwrap_or("не прочитан")));
        let instance_ids = group.members.keys().take(4096).copied().collect::<Vec<_>>();
        ids_in_page += instance_ids.len();
        rows.push(
            json!({"type_id":type_id,"type_name":group.type_name,"family":group.family,
            "level_id":level_id,"level_name":group.level_name,"category_id":category,
            "quantity":count,"example_element_id":first,"instance_ids":instance_ids,
            "instance_ids_complete":count==instance_ids.len(),"source_index":index}),
        );
        sources.push(source);
    }
    let next = (offset + rows.len() < total).then_some(offset + rows.len());
    Ok(envelope(
        TOOL,
        "rvt_counts",
        json!({"schema":"les.model-inventory.v1","mode":"rvt_counts",
        "rows":rows,"total_rows":total,"matched_instances":matched,"offset":offset,"next_offset":next,
        "index_revision":revision,"coverage":"saved_rvt_instances","complete_model":false,
        "source_coverage":saved["coverage"],"unresolved_type_records_in_document":unresolved_type}),
        sources,
    ))
}

pub(super) async fn run(context: Arc<Context>, doc: Value, args: Value) -> Result<Value> {
    tokio::task::spawn_blocking(move || {
        let (records, revision) = snapshot(&context, &doc, &args)?;
        table(doc, &records, &args, &revision)
    })
    .await
    .map_err(|_| "ENGINEERING_READER_FAILED")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_split_by_exact_level_and_category() {
        let placed = |id, category_id, level_id: Option<u64>| {
            json!({"object_id":format!("elements:{id}:FamilyInstance"),"native_id":id,
                "section":"elements","category_id":category_id,
                "model_context":{"types":[{"native_id":"40","name":"СВН / Тип 40"}],
                    "levels":level_id.map(|id|vec![json!({"native_id":id.to_string(),"name":"Этаж"})]).unwrap_or_default()}})
        };
        let snapshot = json!({"objects":[placed("1",-2008099,Some(12)),placed("2",-2008099,Some(12)),
            placed("3",-2008099,None),placed("4",-2000280,Some(12)),placed("5",-2008099,Some(13))]});
        let doc = json!({"id":"model","file_name":"test.rvt","file_hash":"a".repeat(64)});
        let result = table(
            doc,
            &snapshot,
            &json!({"q":"свн","category_id":-2008099}),
            "rvt:control",
        )
        .unwrap();
        assert_eq!(result["result"]["matched_instances"], 4);
        let rows = result["result"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["level_id"], Value::Null);
        assert_eq!(rows[1]["level_id"], "12");
        assert_eq!(rows[1]["instance_ids"], json!(["1", "2"]));
        assert_eq!(rows[2]["level_id"], "13");
    }
}
