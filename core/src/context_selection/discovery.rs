//! Navigation preserves document hierarchy; previews are never answer evidence.
use serde_json::{json, Value};
use std::collections::HashMap;
pub fn catalog(request: &Value) -> Result<Value, &'static str> {
    let units = request["units"]
        .as_array()
        .filter(|u| u.len() <= 2048)
        .ok_or("CONTEXT_SELECTION_INVALID")?;
    let mut positions = HashMap::new();
    let mut entries: Vec<Value> = Vec::new();
    for u in units {
        if !u["source"].is_object() {
            return Err("CONTEXT_SELECTION_INVALID");
        }
        let group = u
            .get("group")
            .filter(|v| v.is_string())
            .cloned()
            .unwrap_or_else(|| json!(u["source"].to_string()));
        let key = group.to_string();
        let index = if let Some(&index) = positions.get(&key) {
            index
        } else {
            if entries.len() >= 64 {
                return Err("CONTEXT_CATALOG_LIMIT");
            }
            let source = &u["source"];
            let path = source["title"]
                .as_str()
                .or_else(|| source["file_name"].as_str())
                .unwrap_or("");
            let mut parts = path.rsplit(['/', '\\']).take(5).collect::<Vec<_>>();
            parts.reverse();
            let title = parts.join(" / ");
            let i = entries.len();
            positions.insert(key, i);
            entries.push(
                json!({"selection_id":i,"group":group,"source":source,"path":path,"title":title,
                "headings":[],"reading_units":0,"evidence_admissible":false}),
            );
            i
        };
        let e = &mut entries[index];
        e["reading_units"] = json!(e["reading_units"].as_u64().unwrap() + 1);
        if let Some(heading) = u["heading"].as_str().filter(|h| !h.trim().is_empty()) {
            let list = e["headings"].as_array_mut().unwrap();
            if list.len() < 3 && !list.iter().any(|v| v == heading) {
                list.push(json!(heading));
            }
        }
    }
    Ok(json!({"schema":"les.source-catalog.v1","entries":entries,"evidence_admissible":false}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_units_do_not_hide_a_later_document_or_erase_its_hierarchy() {
        let mut units:Vec<_>=(0..100).map(|i|json!({"group":"first","source":{"title":"manual/cover.docx"},"heading":format!("Heading {i}")})).collect();
        units.push(json!({"group":"later","source":{"title":"Design/Engineering/Heating/Calculations/heat.docx"},"heading":"Calculated loads"}));
        let c = catalog(&json!({"units":units})).unwrap();
        assert_eq!(c["entries"].as_array().unwrap().len(), 2);
        assert_eq!(c["entries"][1]["group"], "later");
        assert!(c["entries"][1]["title"]
            .as_str()
            .unwrap()
            .contains("Heating"));
        assert_eq!(c["entries"][1]["evidence_admissible"], false);
    }
}
