//! Bounded advisory projection. Documentary evidence has a different owner.
use super::*;

pub(super) fn records(documents: &[Value], project: i64) -> Value {
    let mut records = Vec::new();
    let mut remaining = 5000;
    let mut omitted = 0;
    for document in documents {
        for (index, paragraph) in document["text"]
            .as_str()
            .unwrap_or("")
            .split("\n\n")
            .enumerate()
        {
            if paragraph.trim().is_empty() {
                continue;
            }
            let size = paragraph.chars().count();
            if records.len() >= 8 || size > 1500 || size > remaining {
                omitted += 1;
                continue;
            }
            let path = document["path"].as_str().unwrap_or("");
            records.push(json!({"item_id":format!("agent-file:{path}:{index}"),
                "payload":{"text":paragraph,"source":path,"is_evidence":false},
                "revision_ref":document["revision"],"project_id":document["project_id"]}));
            remaining -= size;
        }
    }
    if omitted > 0 {
        records.push(json!({"item_id":format!("agent-memory-omitted:{project}"),
            "payload":{"omitted_paragraphs":omitted,"read_tool":"agent_workspace",
                "instruction":"При необходимости дочитайте память через инструмент.","is_evidence":false},
            "revision_ref":null,"project_id":project}));
    }
    json!({"records":records,"omitted":omitted,"is_evidence":false})
}
