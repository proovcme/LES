//! Search projections of immutable companion snapshots. No CAD decoder or viewer state.
use serde_json::{json, Value};
use std::collections::HashSet;

pub const SCHEMA: &str = "les.uvyazka-snapshot.v1";
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
    let objects = value["objects"].as_array().ok_or(INVALID)?;
    if objects.is_empty() {
        return Err("NO_SEARCHABLE_TEXT");
    }
    if objects.len() > MAX_OBJECTS || value["object_count"].as_u64() != Some(objects.len() as u64) {
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
    json!({"provider":"uvyazka","project_id":origin["project_id"],"dataset_id":origin["dataset_id"],
        "object_id":object["object_id"],"native_id":object["native_id"],"section":object["section"],
        "source_sha256":origin["source"]["sha256"],"source_name":origin["source"]["filename"],
        "reader":origin["reader"],"coverage":origin["coverage"],"approval_status":"unreviewed"})
}
/// One card belongs to exactly one native object; chunking must never mix objects.
pub fn cards(snapshot: &Value) -> Result<Vec<Value>> {
    validate(snapshot)?;
    snapshot["objects"].as_array().unwrap().iter().map(|object| {
        let origin = &snapshot["origin"];
        let mut lines = vec![format!("# {} — {}", origin["source"]["filename"].as_str().unwrap(), object["name"].as_str().unwrap_or("Объект")),
            format!("ID: {}; раздел: {}; категория: {}", object["native_id"].as_str().unwrap(), object["section"].as_str().unwrap(), object["category"].as_str().unwrap_or("не указана")),
            "Сохранённые данные Увязки; инженерное решение не утверждено. Непрочитанное не означает отсутствие в оригинале.".into()];
        fields(&origin["coverage"], "Покрытие чтения", &mut lines, 0)?;
        for key in ["identifiers", "properties", "units", "evidence", "value_semantics", "quantity_eligible"] {
            if let Some(value) = object.get(key) { fields(value, key, &mut lines, 0)?; }
        }
        Ok(json!({"text":lines.join("\n"),"model_locator":locator(snapshot,object)}))
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
    let text = cards
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.len() > 24 * 1024 * 1024 {
        return Err("ENGINEERING_SNAPSHOT_LIMIT");
    }
    Ok(Some(json!({"text":text,
        "engineering_cards":cards,"metadata":{"extractor":SCHEMA,"engineering_origin":value["origin"]}})))
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
