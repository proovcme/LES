//! Search projections of immutable companion snapshots. No CAD decoder or viewer state.
use serde_json::{json, Value};
use std::collections::HashSet;

pub const SCHEMA: &str = "les.uvyazka-snapshot.v1";
pub const RECORD_SCHEMA: &str = "les.rvt-record.v1";
pub const DWG_RECORD_SCHEMA: &str = "les.dwg-insert.v1";
pub const MAX_OBJECTS: usize = 5000;
const INVALID: &str = "ENGINEERING_SNAPSHOT_INVALID";
type Result<T> = std::result::Result<T, &'static str>;

pub fn identity(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .ok_or(INVALID)
}
pub fn digest(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or(INVALID)
}

pub fn record_key(dataset: &str, file: &str, sha: &str, section: &str, native_id: &str) -> String {
    indexed_record_key(RECORD_SCHEMA, dataset, file, sha, section, native_id)
}

pub fn indexed_record_key(
    schema: &str,
    dataset: &str,
    file: &str,
    sha: &str,
    section: &str,
    native_id: &str,
) -> String {
    crate::sha256_hex(
        json!([schema, dataset, file, sha, section, native_id])
            .to_string()
            .as_bytes(),
    )
}

pub fn rvt_parameter_line(parameter: &Value) -> Option<String> {
    let id = parameter
        .get("parameter_id")
        .filter(|id| !id.is_null())
        .map(|id| {
            id.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| id.to_string())
        });
    let label = match (parameter["name"].as_str(), id) {
        (Some(name), Some(id)) => format!("{name} (ID {id})"),
        (Some(name), None) => name.to_owned(),
        (None, Some(id)) => format!("ID {id}"),
        _ => return None,
    };
    let value = parameter.get("value")?;
    let display_only =
        value.is_number() && parameter["unit"].is_null() && parameter["unit_id"].is_null();
    let value = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let qualifiers = ["storage_type", "unit", "unit_id"]
        .into_iter()
        .filter_map(|key| parameter[key].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut line = if qualifiers.is_empty() {
        format!("{label}: {value}")
    } else {
        format!("{label} [{qualifiers}]: {value}")
    };
    if let Some(unit) = parameter["display_unit_id"]
        .as_str()
        .filter(|_| display_only)
    {
        line.push_str(&format!(
            " (исходное значение Revit без пересчёта. Формат отображения проекта {unit})"
        ));
    }
    Some(line)
}
/// Validate the entire snapshot before emitting a single searchable object.
pub fn validate(value: &Value) -> Result<()> {
    if value["schema"] != SCHEMA {
        return Err(INVALID);
    }
    let origin = &value["origin"];
    for key in ["project_id", "dataset_id"] {
        identity(&origin[key])?;
    }
    identity(&origin["source"]["filename"])?;
    digest(&origin["source"]["sha256"])?;
    if !origin["reader"].is_object()
        || !origin["coverage"].is_object()
        || !matches!(
            origin["coverage"]["status"].as_str(),
            Some("partial" | "complete")
        )
    {
        return Err(INVALID);
    }
    validate_objects(value, MAX_OBJECTS)
}

fn validate_objects(value: &Value, limit: usize) -> Result<()> {
    let objects = value["objects"].as_array().ok_or(INVALID)?;
    if objects.is_empty() {
        return Err("NO_SEARCHABLE_TEXT");
    }
    if objects.len() > limit || value["object_count"].as_u64() != Some(objects.len() as u64) {
        return Err("ENGINEERING_SNAPSHOT_INCOMPLETE");
    }
    let mut seen = HashSet::new();
    for object in objects {
        let id = identity(&object["object_id"])?;
        identity(&object["section"])?;
        identity(&object["native_id"])?;
        if !seen.insert(id) || !object["properties"].is_object() {
            return Err(INVALID);
        }
        if serde_json::to_vec(object).map_err(|_| INVALID)?.len() > 128 * 1024 {
            return Err("ENGINEERING_OBJECT_LIMIT");
        }
    }
    Ok(())
}
fn fields(value: &Value, path: &str, lines: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > 32 {
        return Err(INVALID);
    }
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                fields(value, &format!("{path} / {key}"), lines, depth + 1)?;
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (index, value) in items.iter().enumerate() {
                fields(value, &format!("{path} [{}]", index + 1), lines, depth + 1)?;
            }
        }
        Value::Null => lines.push(format!("{path}: null (значение не задано)")),
        Value::String(s) => lines.push(format!(
            "{path}: {}",
            if s.is_empty() {
                "(пустая строка)"
            } else {
                s
            }
        )),
        _ => lines.push(format!("{path}: {value}")),
    }
    Ok(())
}
pub fn locator(snapshot: &Value, object: &Value) -> Value {
    let origin = &snapshot["origin"];
    if origin["provider"] == "les" {
        let mut locator = json!({"provider":"les","object_id":object["object_id"],
            "native_id":object["native_id"],"section":object["section"],
            "source_sha256":origin["source"]["sha256"],"source_name":origin["source"]["filename"],
            "identifiers":object["identifiers"],"reader":origin["reader"],
            "coverage":origin["coverage"],"approval_status":"unreviewed"});
        for key in ["drawing_sheet", "model_context"] {
            if let Some(info) = object
                .get(key)
                .filter(|value| value.as_object().is_some_and(|m| !m.is_empty()))
            {
                locator[key] = info.clone();
            }
        }
        if origin["reader"]["format"] == "rvt" && object["saved_parameters"].is_array() {
            locator["record_schema"] = json!(RECORD_SCHEMA);
        }
        if origin["reader"]["format"] == "dwg" && object["dwg_insert"].is_object() {
            locator["record_schema"] = json!(DWG_RECORD_SCHEMA);
        }
        return locator;
    }
    json!({"provider":"uvyazka","project_id":origin["project_id"],"dataset_id":origin["dataset_id"],
        "object_id":object["object_id"],"native_id":object["native_id"],"section":object["section"],
        "source_sha256":origin["source"]["sha256"],"source_name":origin["source"]["filename"],
        "reader":origin["reader"],"coverage":origin["coverage"],"approval_status":"unreviewed"})
}
/// One card belongs to exactly one native object; chunking must never mix objects.
pub fn cards(snapshot: &Value) -> Result<Vec<Value>> {
    validate(snapshot)?;
    project_cards(snapshot)
}

fn project_cards(snapshot: &Value) -> Result<Vec<Value>> {
    snapshot["objects"].as_array().unwrap().iter().map(|object| {
        let origin = &snapshot["origin"];
        let mut lines = vec![format!("# {} — {}", origin["source"]["filename"].as_str().unwrap(), object["name"].as_str().unwrap_or("Объект")),
            format!("ID: {}; раздел: {}; категория: {}", object["native_id"].as_str().unwrap(), object["section"].as_str().unwrap(), object["category"].as_str().unwrap_or("не указана")),
            if origin["provider"] == "les" {
                "Сохранённые свойства объекта; прочитана часть файла.".into()
            } else {
                "Сохранённые данные Увязки; инженерное решение не утверждено. Непрочитанное не означает отсутствие в оригинале.".into()
            }];
        let sheet = &object["drawing_sheet"];
        if let Some(name) = sheet["name"].as_str() {
            let mut scope = format!("Лист DWG: {name}");
            let caption = &sheet["prominent_text"];
            if caption["handle"] != object["native_id"] {
                if let Some(label) = caption["text"].as_str() {
                    scope.push_str(&format!(". Крупная надпись на листе: {label}"));
                }
            }
            lines.insert(1, scope);
        }
        let mut context_lines = Vec::new();
        for (kind, label) in [("levels", "Уровень"), ("spaces", "Помещение"), ("systems", "Система"), ("types", "Тип"), ("families", "Семейство")] {
            if let Some(items) = object["model_context"][kind].as_array() {
                for item in items {
                    if let (Some(name), Some(id)) = (item["name"].as_str(), item["native_id"].as_str()) {
                        context_lines.push(format!("{label}: {name} (ID {id})"));
                    }
                }
            }
        }
        let summary = if origin["provider"] == "les" && origin["reader"]["format"] == "rvt"
            && object["section"] == "elements" && !context_lines.is_empty() {
            let label = object["name"].as_str().or_else(|| object["model_context"]["types"][0]["name"].as_str());
            label.map(|label| format!("Объект Revit: {label}\n{}\nElementId: {}",
                context_lines.join("\n"), object["native_id"].as_str().unwrap()))
                .filter(|summary| summary.chars().count() <= 480)
        } else {
            None
        };
        lines.extend(context_lines);
        if origin["provider"] != "les" {
            fields(&origin["coverage"], "Покрытие чтения", &mut lines, 0)?;
        }
        for key in ["identifiers", "properties", "units", "evidence", "value_semantics", "quantity_eligible"] {
            if let Some(value) = object.get(key) { fields(value, key, &mut lines, 0)?; }
        }
        let mut card = json!({"model_locator":locator(snapshot,object),"relations_text":summary});
        if origin["provider"] == "les" && origin["reader"]["format"] == "rvt" {
            if let Some(parameters) = object["saved_parameters"].as_array() {
                for (i, line) in parameters.iter().filter_map(rvt_parameter_line).enumerate() {
                    lines.push(format!("properties / parameters [{}]: {line}", i + 1));
                }
                card["indexed_engineering_record"] = json!({
                    "schema": RECORD_SCHEMA, "name": object["name"],
                    "properties": object["properties"], "parameters": parameters
                });
            }
        }
        if card["model_locator"]["record_schema"] == DWG_RECORD_SCHEMA {
            let mut insert = object["dwg_insert"].clone();
            insert["schema"] = json!(DWG_RECORD_SCHEMA);
            card["indexed_engineering_record"] = insert;
        }
        card["text"] = json!(lines.join("\n"));
        Ok(card)
    }).collect()
}
pub fn convert(bytes: &[u8]) -> Result<Option<Value>> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(None);
    };
    if value["schema"] != SCHEMA {
        return Ok(None);
    }
    let cards = cards(&value)?;
    Ok(Some(content(SCHEMA, &value["origin"], cards)?))
}

pub(crate) fn native_content(filename: &str, sha: &str, decoded: &Value) -> Result<Value> {
    let mut snapshot = decoded.clone();
    if snapshot["schema"] != "les.engineering-reader.v1"
        || snapshot["coverage"]["status"] != "partial"
        || !snapshot["reader"].is_object()
    {
        return Err(INVALID);
    }
    snapshot["origin"] = json!({"provider":"les","source":{"filename":filename,"sha256":sha},
        "reader":decoded["reader"],"coverage":decoded["coverage"]});
    validate_objects(&snapshot, 20_000)?;
    let cards = project_cards(&snapshot)?;
    content("les.engineering-reader.v1", &snapshot["origin"], cards)
}

fn content(schema: &str, origin: &Value, cards: Vec<Value>) -> Result<Value> {
    let text = cards
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.len() > 24 * 1024 * 1024 {
        return Err("ENGINEERING_SNAPSHOT_LIMIT");
    }
    Ok(json!({"text":text,
        "engineering_cards":cards,"metadata":{"extractor":schema,"engineering_origin":origin}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rvt_relationship_text_uses_the_resolved_type_and_saved_systems() {
        let model = json!({"schema":"les.engineering-reader.v1","coverage":{"status":"partial"},
        "reader":{"format":"rvt"},"object_count":1,"objects":[{
            "object_id":"elements:42","section":"elements","native_id":"42","name":null,
            "properties":{"mass":12.5},
            "model_context":{"types":[{"native_id":"27","name":"Бойлер"}],
                "systems":[{"native_id":"66","name":"В1"},{"native_id":"67","name":"Т3"}]}
        }]});
        let content = native_content("model.rvt", &"a".repeat(64), &model).unwrap();
        let card = &content["engineering_cards"][0];
        assert_eq!(card["relations_text"],
            "Объект Revit: Бойлер\nСистема: В1 (ID 66)\nСистема: Т3 (ID 67)\nТип: Бойлер (ID 27)\nElementId: 42");
        assert!(card["text"]
            .as_str()
            .unwrap()
            .contains("properties / mass: 12.5"));
    }

    #[test]
    fn native_ifc_names_and_relationship_ids_reach_the_search_card() {
        let hash = "b".repeat(64);
        let model = json!({"schema":"les.engineering-reader.v1","coverage":{"status":"partial"},
        "reader":{"format":"ifc"},"object_count":1,"objects":[{
            "object_id":"elements:42","section":"elements","native_id":"#42","name":"Насос",
            "identifiers":{"step_id":42,"global_id":"ifc-guid"},
            "properties":{"Марка":"Н3"},
            "model_context":{"levels":[{"native_id":"#20","name":"Подвал","via":["#71","#72"]}],
                "types":[{"native_id":"#30","via":["#73"]}]}
        }]});
        let content = native_content("model.ifc", &hash, &model).unwrap();
        let card = &content["engineering_cards"][0];
        let text = card["text"].as_str().unwrap();
        assert!(text.contains("Уровень: Подвал (ID #20)") && text.contains("Марка: Н3"));
        assert!(!text.contains("Тип:"));
        assert_eq!(card["model_locator"]["identifiers"]["step_id"], 42);
        assert_eq!(
            card["model_locator"]["model_context"]["levels"][0]["via"],
            json!(["#71", "#72"])
        );
    }

    #[test]
    fn native_dwg_card_includes_sheet_text_with_its_own_source_handle() {
        let hash = "a".repeat(64);
        let decoded = json!({"schema":"les.engineering-reader.v1","object_count":1,
            "coverage":{"status":"partial"},"reader":{"format":"dwg"},"objects":[
            {"object_id":"elements:22","section":"elements","native_id":"22","properties":{"text":"Расход 90 м³/ч"},
             "drawing_sheet":{"handle":"11","name":"P01","prominent_text":{"handle":"21","text":"Контуры вентиляции"}}}]});
        let converted = native_content("drawing.dwg", &hash, &decoded).unwrap();
        let card = &converted["engineering_cards"][0];
        let text = card["text"].as_str().unwrap();
        assert!(text.contains("Лист DWG: P01. Крупная надпись на листе: Контуры вентиляции"));
        assert!(text.contains("properties / text: Расход 90 м³/ч"));
        assert_eq!(card["model_locator"]["native_id"], "22");
        assert_eq!(
            card["model_locator"]["drawing_sheet"],
            decoded["objects"][0]["drawing_sheet"]
        );
        assert_eq!(card["model_locator"]["source_sha256"], hash);
    }

    fn sample() -> Value {
        json!({"schema":SCHEMA,"origin":{"project_id":"example","dataset_id":"snapshot-1",
        "source":{"filename":"Example.rvt","sha256":"a".repeat(64)},"reader":{"engine":"native","revision":"1"},"coverage":{"status":"partial","limitations":["Geometry not extracted"]}},
        "object_count":2,"objects":[{"object_id":"elements:1","native_id":"1","section":"elements","name":"Щит","properties":{"power":{"value":0,"unit":"kW"},"missing":null}},
        {"object_id":"elements:2","native_id":"2","section":"elements","name":"Кабель","properties":{"length":{"value":12.5,"unit":"m"}}}]})
    }
    #[test]
    fn cards_preserve_identity_units_zero_missing_and_coverage() {
        let cards = cards(&sample()).unwrap();
        let a = cards[0]["text"].as_str().unwrap();
        assert!(
            a.contains("value: 0")
                && a.contains("unit: kW")
                && a.contains("null (значение не задано)")
                && a.contains("Geometry not extracted")
        );
        assert!(!a.contains("12.5"));
        assert_eq!(cards[1]["model_locator"]["object_id"], "elements:2");
        assert_eq!(cards[1]["model_locator"]["source_sha256"], "a".repeat(64));
    }
    #[test]
    fn incomplete_duplicate_or_empty_exports_cannot_be_indexed() {
        let mut v = sample();
        v["object_count"] = json!(3);
        assert!(cards(&v).is_err());
        v = sample();
        v["objects"][1]["object_id"] = json!("elements:1");
        assert!(cards(&v).is_err());
        v = sample();
        v["objects"] = json!([]);
        v["object_count"] = json!(0);
        assert_eq!(cards(&v).unwrap_err(), "NO_SEARCHABLE_TEXT");
    }
    #[test]
    fn native_conversion_keeps_the_original_and_rejects_a_changed_revision() {
        let dir = std::env::temp_dir().join(format!("les-cad-conversion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Пример.dwg");
        let original = b"original drawing";
        std::fs::write(&path, original).unwrap();
        let hash = crate::sha256_hex(original);
        let request = json!({"schema":"les.document-convert.v1","path":path,"source_sha256":hash});
        let decoded = json!({"schema":"les.engineering-reader.v1","reader":{"engine":"fixture"},"coverage":{"status":"partial"},
            "object_count":1,"objects":[{"object_id":"elements:1ED","native_id":"1ED","section":"elements","name":"Светильник",
                "properties":{"group":"L3"},"identifiers":{"handle":"1ED"}}]});
        let output = crate::document::conversion::execute_with(&request, &|input| {
            assert_eq!(input["path"], request["path"]);
            assert_eq!(input["source_sha256"], hash);
            Ok(decoded.clone())
        })
        .unwrap();
        let card = &output["engineering_cards"][0];
        assert!(card["text"].as_str().unwrap().contains("group: L3"));
        assert_eq!(card["model_locator"]["provider"], "les");
        assert_eq!(card["model_locator"]["source_name"], "Пример.dwg");
        assert_eq!(card["model_locator"]["source_sha256"], hash);
        assert_eq!(card["model_locator"]["identifiers"]["handle"], "1ED");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let changed = crate::document::conversion::execute_with(&request, &|_| {
            std::fs::write(&path, b"changed drawing").unwrap();
            Ok(decoded.clone())
        });
        assert_eq!(changed.unwrap_err(), "SOURCE_CHANGED_DURING_INDEXING");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
