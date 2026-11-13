use super::*;
use std::collections::BTreeSet;
fn key(source: &Value) -> [String; 3] {
    if truth(&source["context_unit"]) && source["document_fragment_count"].as_u64().is_some() {
        return [
            string(&source["dataset_id"]),
            string(&source["doc_name"]),
            string(&source["context_unit"]),
        ];
    }
    ["dataset_id", "doc_name", "parent_id"].map(|field| {
        if truth(&source[field]) {
            string(&source[field])
        } else {
            String::new()
        }
    })
}
pub(super) fn report(sources: &[Value], visible: &[Value], compact: bool) -> Value {
    let mut groups: Vec<([String; 3], BTreeSet<Option<u64>>)> = Vec::new();
    for source in sources
        .iter()
        .filter(|s| s.is_object() && truth(&s["parent_id"]))
    {
        let key = key(source);
        let index = groups
            .iter()
            .position(|(k, _)| *k == key)
            .unwrap_or_else(|| {
                groups.push((key, BTreeSet::new()));
                groups.len() - 1
            });
        let count = if truth(&source["context_unit"]) {
            &source["document_fragment_count"]
        } else {
            &source["section_fragment_count"]
        }
        .as_u64()
        .filter(|n| *n > 0);
        groups[index].1.insert(count);
    }
    let rows:Vec<_>=groups.into_iter().map(|(group,counts)|{
        let total=if counts.len()==1{counts.first().copied().flatten()}else{None};
        let included:BTreeSet<_>=visible.iter().filter(|s|s.is_object() && truth(&s["qdrant_point_id"]) && key(s)==group)
            .map(|s|string(&s["qdrant_point_id"])).collect();
        let count=included.len() as u64;
        let valid=total.filter(|n|count<=*n);
        let mut row=json!({"indexed_fragments":total,"model_fragments":count,
            "omitted_fragments":valid.map(|n|n-count),"status":match valid{Some(n)if n==count=>"complete",Some(_)=>"partial",None=>"unknown"}});
        if compact {
            row["source_indexes"]=json!(sources.iter().filter(|s|s.is_object() && s.get("index").is_some() && key(s)==group)
                .map(|s|s["index"].clone()).collect::<Vec<_>>());
        } else {
            for (field,value) in ["dataset_id","doc_name","parent_id"].iter().zip(group){row[*field]=json!(value);}
        }
        row
    }).collect();
    json!({"schema":"les.model-section-coverage.v1","sections":rows,
        "scope":"indexed fragments only; completeness of the original, OCR and answerability are not established"})
}
pub(super) fn reservation(sources: &[Value]) -> Option<Value> {
    let report = report(sources, &[], true);
    let count = list(&report, "sections").len();
    if count == 0 {
        return None;
    }
    let max = render(&report).chars().count().max(
        render(&self::report(sources, sources, true))
            .chars()
            .count(),
    );
    Some(object(COVERAGE_ID, json!(" ".repeat(max + 64 * count))))
}
pub(super) fn finalize(mut packet: Value, sources: &[Value], visible: &[Value]) -> Result<Value> {
    let replacement = object(COVERAGE_ID, report(sources, visible, true));
    let mut delta = 0;
    for section in packet["sections"].as_array_mut().ok_or(ERROR)? {
        if section["kind"] != "request" {
            continue;
        }
        let mut change = 0;
        for item in section["objects"].as_array_mut().ok_or(ERROR)? {
            if item["object_id"] == COVERAGE_ID {
                let difference =
                    tokens(&object_render(&replacement)) - tokens(&object_render(item));
                if difference > 0 {
                    return Err("SECTION_COVERAGE_RESERVATION_EXCEEDED");
                }
                change += difference;
                *item = replacement.clone();
            }
        }
        section["token_count"] = json!(section["token_count"].as_i64().ok_or(ERROR)? + change);
        delta += change;
    }
    packet["included_tokens"] = json!(packet["included_tokens"].as_i64().ok_or(ERROR)? + delta);
    Ok(packet)
}
