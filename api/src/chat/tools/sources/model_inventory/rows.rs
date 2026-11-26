//! Project instance/type records to traceable rows and aggregate in Rust.
use super::{envelope, source, ModelRows, Result, TOOL};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const NAME: &[&str] = &[
    "adsk наименование",
    "наименование",
    "adsk наименование краткое",
];
const BRAND: &[&str] = &["adsk марка", "adsk марка изделия", "марка", "модель"];
const ARTICLE: &[&str] = &["adsk артикул", "артикул", "adsk код изделия", "код изделия"];
const MAKER: &[&str] = &[
    "adsk завод-изготовитель",
    "завод-изготовитель",
    "изготовитель",
    "производитель",
    "manufacturer",
];
const SUPPLIER: &[&str] = &["adsk поставщик", "поставщик", "supplier"];
const NOMINAL: &[&str] = &[
    "adsk номинальная мощность",
    "номинальная мощность",
    "rated power",
];
const APPARENT: &[&str] = &["adsk полная мощность", "полная мощность", "apparent power"];

fn label_key(value: &str) -> String {
    value
        .to_lowercase()
        .replace('ё', "е")
        .replace('_', " ")
        .replace(['–', '—'], "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn locator(row: &Value) -> &Value {
    &row["payload"]["model_locator"]
}

fn record(row: &Value) -> &Value {
    &row["payload"]["indexed_engineering_record"]
}

fn filled(value: &Value) -> bool {
    value.is_number() || value.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn field(instance: &Value, typ: Option<&Value>, names: &[&str]) -> Value {
    for (scope, row) in std::iter::once(("instance", instance)).chain(typ.map(|r| ("type", r))) {
        let parameters = record(row)["parameters"].as_array().into_iter().flatten();
        let mut variants: BTreeMap<(usize, String), &Value> = BTreeMap::new();
        for p in parameters {
            if let Some(rank) = p["name"]
                .as_str()
                .and_then(|name| names.iter().position(|n| *n == label_key(name)))
            {
                if filled(&p["value"]) {
                    let identity = json!([p["value"], p["spec_type_id"], p["unit_id"], p["unit"]]);
                    variants.insert((rank, identity.to_string()), p);
                }
            }
        }
        if let Some(((rank, _), p)) = variants.first_key_value() {
            let state = if variants.keys().filter(|(r, _)| r == rank).count() == 1 {
                "saved"
            } else {
                "conflict"
            };
            return json!({"state":state,"value":if state=="saved" {p["value"].clone()}else{Value::Null},
                "origin":scope,"element_id":locator(row)["native_id"],"parameter_id":p["parameter_id"],
                "spec_type_id":p["spec_type_id"],"unit_id":p["unit_id"],"unit":p["unit"],
                "display_unit_id":p["display_unit_id"],"storage_type":p["storage_type"]});
        }
    }
    json!({"state":"missing"})
}

fn quantity(mut field: Value, apparent: bool) -> Value {
    let unit = if apparent { "ВА" } else { "Вт" };
    field["normalized_unit"] = json!(unit);
    if field["state"] != "saved" {
        return field;
    }
    let spec = if apparent {
        "autodesk.spec.aec.electrical:apparentPower"
    } else {
        "autodesk.spec.aec.electrical:power"
    };
    let typed = field["spec_type_id"]
        .as_str()
        .and_then(|s| s.rsplit_once('-'))
        .is_some_and(|(s, _)| s == spec)
        && field["storage_type"] == "Double";
    let factor = match field["unit_id"]
        .as_str()
        .and_then(|s| s.rsplit_once('-'))
        .map(|(id, _)| id)
    {
        None if field["unit"].is_null() && field["unit_id"].is_null() => Some(0.3048_f64.powi(2)),
        Some("autodesk.unit.unit:watts") if !apparent => Some(1.0),
        Some("autodesk.unit.unit:kilowatts") if !apparent => Some(1000.0),
        Some("autodesk.unit.unit:voltAmperes") if apparent => Some(1.0),
        Some("autodesk.unit.unit:kilovoltAmperes") if apparent => Some(1000.0),
        _ => None,
    };
    let value = field["value"]
        .as_f64()
        .zip(factor)
        .map(|(value, factor)| value * factor)
        .filter(|v| typed && v.is_finite() && *v >= 0.0);
    field["normalized_value"] = json!(value);
    field["state"] = json!(if value.is_some() {
        "converted"
    } else {
        "unverified_unit_or_value"
    });
    field
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn compact(value: &Value) -> Value {
    if let Some(s) = text(value) {
        json!(s.chars().take(160).collect::<String>())
    } else {
        value.clone()
    }
}

fn names(row: &Value, group: &str) -> Vec<Value> {
    locator(row)["model_context"][group]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|link| {
            text(&link["name"])
                .map(|name| json!({"name":compact(&json!(name)),"element_id":link["native_id"]}))
        })
        .collect()
}

struct Item<'a> {
    doc: &'a Value,
    indexed: &'a Value,
    row: Value,
    labels: String,
    typed: bool,
}

fn build<'a>(model: &'a ModelRows, system: Option<&str>) -> Vec<Item<'a>> {
    let types: BTreeMap<_, _> = model
        .records
        .iter()
        .filter(|row| locator(row)["section"] == "types")
        .filter_map(|row| locator(row)["native_id"].as_str().map(|id| (id, row)))
        .collect();
    model.records.iter().filter(|row| locator(row)["section"] == "elements").filter_map(|row| {
        let loc = locator(row);
        let systems = names(row, "systems");
        if system.is_some_and(|wanted| !systems.iter().any(|link| text(&link["name"]).is_some_and(|s| label_key(s) == wanted))) {
            return None;
        }
        let links = loc["model_context"]["types"].as_array().map(Vec::as_slice).unwrap_or_default();
        let type_id = (links.len() == 1).then(|| &links[0]["native_id"]).and_then(Value::as_str);
        let typ = type_id.and_then(|id| types.get(id)).copied();
        let type_name = links.first().and_then(|link| text(&link["name"]));
        let label = field(row, typ, NAME);
        let name = text(&label["value"]).or_else(|| text(&record(row)["name"])).or(type_name);
        let brand = field(row, typ, BRAND);
        let article = field(row, typ, ARTICLE);
        let maker = field(row, typ, MAKER);
        let supplier = field(row, typ, SUPPLIER);
        let family = names(row, "families");
        let labels = [name, type_name, text(&record(row)["name"]), text(&brand["value"]), text(&article["value"]), text(&maker["value"])]
            .into_iter().flatten().chain(family.iter().filter_map(|link| text(&link["name"]))).collect::<Vec<_>>().join(" ");
        let mut fields = json!({"name":label,"brand":brand,"article":article,"manufacturer":maker,"supplier":supplier});
        for field in fields.as_object_mut().unwrap().values_mut() {
            field["value"] = compact(&field["value"]);
        }
        let values = fields.as_object().unwrap().iter().map(|(key, field)| (key.clone(), field["value"].clone())).collect::<Map<_,_>>();
        let mut output = Value::Object(values);
        output["name"] = compact(&json!(name));
        output["fields"] = fields;
        output["element_id"] = loc["native_id"].clone();
        output["type_id"] = json!(type_id);
        output["type_name"] = compact(&json!(type_name));
        output["type_resolution"] = json!(if links.len() > 1 { "ambiguous" } else if typ.is_some() { "resolved" } else { "missing" });
        output["levels"] = json!(names(row,"levels"));
        output["spaces"] = json!(names(row,"spaces"));
        output["systems"] = json!(systems);
        output["nominal_power"] = quantity(field(row,typ,NOMINAL),false);
        output["apparent_power"] = quantity(field(row,typ,APPARENT),true);
        output["doc_id"] = model.doc["id"].clone();
        let typed = !links.is_empty() || loc["object_id"].as_str().is_some_and(|id| id.ends_with(":FamilyInstance"));
        Some(Item {doc:&model.doc,indexed:row,row:output,labels,typed})
    }).collect()
}

fn terms(content: &[String]) -> Result<Vec<BTreeSet<String>>> {
    let normalized = les_core::lexical::reading(content)?;
    Ok(normalized["documents"]
        .as_array()
        .ok_or("INDEX_READ_INVALID")?
        .iter()
        .map(|doc| {
            doc["terms"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned())
                .collect()
        })
        .collect())
}

fn selected<'a>(models: &'a [ModelRows], args: &Value, mode: &str) -> Result<Vec<Item<'a>>> {
    let system = text(&args["system"]).map(label_key);
    let mut items = models
        .iter()
        .flat_map(|model| build(model, system.as_deref()))
        .filter(|item| mode != "vendors" || item.typed)
        .collect::<Vec<_>>();
    if let Some(query) = text(&args["q"]) {
        let wanted = terms(&[label_key(query)])?
            .pop()
            .ok_or("INDEX_QUERY_INVALID")?;
        if wanted.is_empty() {
            return Err("INDEX_QUERY_INVALID".into());
        }
        let mut matches = Vec::new();
        for batch in items.chunks(128) {
            let labels: Vec<_> = batch
                .iter()
                .map(|item| label_key(&item.labels).chars().take(1800).collect())
                .collect();
            matches.extend(terms(&labels)?.iter().map(|found| wanted.is_subset(found)));
        }
        let mut matches = matches.into_iter();
        items.retain(|_| matches.next() == Some(true));
    }
    items.sort_by(|left, right| {
        left.doc["file_name"]
            .as_str()
            .cmp(&right.doc["file_name"].as_str())
            .then_with(|| left.doc["id"].as_str().cmp(&right.doc["id"].as_str()))
            .then_with(|| numeric_id(&left.row).cmp(&numeric_id(&right.row)))
    });
    Ok(items)
}

fn numeric_id(row: &Value) -> u64 {
    row["element_id"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .unwrap_or(u64::MAX)
}

fn position(item: &Item<'_>) -> String {
    let row = &item.row;
    // Join identical articles only when their manufacturer and material fields agree.
    let fields = [
        &row["manufacturer"],
        &row["article"],
        &row["brand"],
        &row["supplier"],
    ];
    if filled(fields[0]) && filled(fields[1]) {
        return json!([
            "article",
            text(fields[0]).map(label_key),
            fields[1],
            fields[2],
            fields[3]
        ])
        .to_string();
    }
    json!([
        "type",
        row["doc_id"],
        row["type_id"]
            .as_str()
            .unwrap_or_else(|| row["element_id"].as_str().unwrap_or_default()),
        row["name"],
        fields
    ])
    .to_string()
}

fn total(items: &[Item<'_>], field: &str) -> Result<Value> {
    let mut sum = 0.0_f64;
    let mut known = 0;
    let mut conflicts = 0;
    for item in items {
        if let Some(number) = item.row[field]["normalized_value"].as_f64() {
            sum += number;
            known += 1;
        } else if item.row[field]["state"] == "conflict" {
            conflicts += 1;
        }
    }
    if !sum.is_finite() {
        return Err("MODEL_INVENTORY_LIMIT".into());
    }
    Ok(
        json!({"sum":(known>0).then_some(sum),"known":known,"unknown":items.len()-known,"conflicts":conflicts}),
    )
}

pub(super) fn table(models: Vec<ModelRows>, args: &Value, revision: Value) -> Result<Value> {
    let mode = args["mode"].as_str().ok_or("TOOL_SCHEMA_INVALID")?;
    let mut items = selected(&models, args, mode)?;
    if mode == "power" {
        items.sort_by_key(|item| {
            item.row["nominal_power"]["normalized_value"].is_null()
                && item.row["apparent_power"]["normalized_value"].is_null()
        });
    }
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 24) as usize;
    let mut sources = Vec::new();
    let mut rows = Vec::new();
    let count;
    if mode == "vendors" {
        let mut groups: BTreeMap<String, (usize, &Item<'_>)> = BTreeMap::new();
        for item in &items {
            groups
                .entry(position(item))
                .and_modify(|group| group.0 += 1)
                .or_insert((1, item));
        }
        count = groups.len();
        for (quantity, item) in groups.into_values().skip(offset).take(limit) {
            let index = sources.len() + 1;
            let mut evidence = source(item.doc, item.indexed);
            evidence["index"] = json!(index);
            sources.push(evidence);
            rows.push(json!({"name":item.row["name"],"type_name":item.row["type_name"],"type_id":item.row["type_id"],"brand":item.row["brand"],
                "article":item.row["article"],"manufacturer":item.row["manufacturer"],"supplier":item.row["supplier"],
                "fields":item.row["fields"],"quantity":quantity,"example_element_id":item.row["element_id"],"source_index":index}));
        }
    } else {
        count = items.len();
        for item in items.iter().skip(offset).take(limit) {
            let index = sources.len() + 1;
            let mut evidence = source(item.doc, item.indexed);
            evidence["index"] = json!(index);
            sources.push(evidence);
            let mut row = item.row.clone();
            row["source_index"] = json!(index);
            rows.push(row);
        }
    }
    let matched = items.len();
    let power = if mode == "power" {
        json!({"nominal_w":total(&items,"nominal_power")?,"apparent_va":total(&items,"apparent_power")?})
    } else {
        Value::Null
    };
    let model_coverage: Vec<_> = models.iter().map(|model| {
        let instances = model.records.iter().filter(|r|locator(r)["section"]=="elements").count();
        json!({"doc_id":model.doc["id"],"doc_name":model.doc["file_name"],"source_sha256":model.doc["file_hash"],
            "status":if model.doc["status"] != "INDEXED" { "not_indexed" } else if model.records.is_empty() { "records_unavailable" } else { "read" },
            "extracted_instances":instances,"structured_records":model.records.len(),
            "reader_coverage":model.records.first().map(|r| &locator(r)["coverage"])})
    }).collect();
    let next = (offset + rows.len() < count).then_some(offset + rows.len());
    Ok(envelope(
        TOOL,
        mode,
        json!({"schema":"les.model-inventory.v1","mode":mode,"q":args["q"],"system":args["system"],
        "rows":rows,"offset":offset,"next_offset":next,"total_rows":count,"matched_instances":matched,"power":power,
        "index_revision":revision,"coverage":"extracted_rvt_records","complete_model":false,"models":model_coverage,
        "vendor_scope":(mode=="vendors").then_some("typed_instances")}),
        sources,
    ))
}
