//! A bounded model view of discovery; full search receipts stay in the journal.
use serde_json::{json, Value};
pub(super) fn project(payload: &Value, context_chars: usize) -> Value {
    let mut value = payload.clone();
    if value["status"] != "ok" {
        return value;
    }
    if value["operation"] == "call"
        && value["tool"]
            .as_str()
            .is_some_and(|s| s.starts_with("use_extensions_"))
    {
        value["result"] = project(&value["result"], context_chars);
        return value;
    }
    if !matches!(
        value["tool"].as_str(),
        Some("search_sources" | "dataset_map")
    ) {
        return value;
    }
    value.as_object_mut().unwrap().remove("inputs");
    let catalog = value["result"]["documents"].is_array();
    let field = if catalog {
        "documents"
    } else {
        "candidate_documents"
    };
    let rows = value["result"][field]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut result = json!({"evidence_admissible":false,"coverage":"bounded","total_candidates":rows.len(),
        "note":"Discovery metadata only. Read the selected document before answering. Refine the query if the requested document is absent."});
    result[field] = json!([]);
    if catalog {
        for key in ["total", "offset", "next_offset"] {
            result[key] = value["result"][key].clone();
        }
    } else {
        for key in ["total_candidates", "offset", "next_offset"] {
            if value["result"].get(key).is_some() {
                result[key] = value["result"][key].clone();
            }
        }
    }
    // Half the available chars remain for the provider call, stage control and
    // source coverage. Page boundaries never skip hidden catalog entries.
    let cap = (context_chars / 2).max(512);
    let mut shown = Vec::new();
    for row in &rows {
        let mut card = serde_json::Map::new();
        for key in [
            if catalog { "id" } else { "doc_id" },
            "dataset_id",
            "source_sha256",
            "page",
            "status",
        ] {
            if !row[key].is_null() {
                card.insert(key.into(), row[key].clone());
            }
        }
        card.insert(
            "file_name".into(),
            json!(row["file_name"]
                .as_str()
                .unwrap_or("")
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")),
        );
        card.insert(
            "path".into(),
            row.get("path")
                .cloned()
                .unwrap_or_else(|| row["file_name"].clone()),
        );
        shown.push(Value::Object(card));
        result[field] = json!(shown);
        if result.to_string().chars().count() > cap {
            shown.pop();
            result[field] = json!(shown);
            break;
        }
    }
    result["included_candidates"] = json!(shown.len());
    if catalog {
        let next = value["result"]["offset"].as_u64().unwrap_or(0) + shown.len() as u64;
        result["next_offset"] = if next < value["result"]["total"].as_u64().unwrap_or(0) {
            json!(next)
        } else {
            Value::Null
        };
    }
    if shown.len() == rows.len() {
        result["coverage"] = json!("page_complete");
    }
    value["result"] = result;
    value["sources"] = json!([]);
    if value["execution"].is_object() {
        value["execution"] =
            json!({"status":value["execution"]["status"],"code":value["execution"]["code"]});
    }
    value
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_catalog_page_is_bounded_without_skipping_hidden_rows_or_claiming_evidence() {
        let raw = json!({"tool":"search_sources","status":"ok","result":{"documents":(0..50).map(|i|json!({"id":i.to_string(),"dataset_id":"ds","file_name":format!("{}/Source-{i}.pdf","folder/".repeat(40))})).collect::<Vec<_>>(),"total":100,"offset":7,"next_offset":57},"sources":[]});
        let before = raw.clone();
        let projected = project(&raw, 2400);
        let result = &projected["result"];
        let count = result["documents"].as_array().unwrap().len();
        assert!(count > 0 && count < 50);
        assert_eq!(result["next_offset"], 7 + count);
        assert_eq!(result["evidence_admissible"], false);
        assert_eq!(
            result["documents"][0]["path"],
            raw["result"]["documents"][0]["file_name"]
        );
        assert_eq!(result["documents"][0]["file_name"], "Source-0.pdf");
        assert_eq!(raw, before);
    }
    #[test]
    fn passage_search_does_not_leak_answer_text_into_discovery_and_errors_stay_exact() {
        let raw = json!({"tool":"search_sources","status":"ok","result":{"passages":[{"text":"UNREAD FACT"}],"candidate_documents":[{"doc_id":"doc","dataset_id":"ds","file_name":"Book.xlsx"}]},"sources":[{"text":"UNREAD FACT"}]});
        let projected = project(&raw, 4000);
        assert!(!projected.to_string().contains("UNREAD FACT"));
        assert_eq!(
            projected["result"]["candidate_documents"][0]["doc_id"],
            "doc"
        );
        let error = json!({"status":"error","tool":"search_sources","result":{"code":"INDEX_CHANGED_DURING_SEARCH"}});
        assert_eq!(project(&error, 4000), error);
    }
}
