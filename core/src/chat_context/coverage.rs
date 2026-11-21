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
/// Coverage can use the space left by real readings. Its cost includes the
/// separator that the common packer charges for an additional object.
pub(super) fn for_window(sources: &[Value], visible: &[Value], spare: i64) -> Option<Value> {
    let groups: BTreeSet<_> = visible
        .iter()
        .filter(|s| s.is_object() && truth(&s["parent_id"]))
        .map(key)
        .collect();
    if groups.is_empty() || spare <= 1 {
        return None;
    }
    let relevant: Vec<_> = sources
        .iter()
        .filter(|s| groups.contains(&key(s)))
        .cloned()
        .collect();
    let mut detail = report(&relevant, visible, true);
    let rows = detail["sections"].as_array_mut()?;
    for row in rows.iter_mut() {
        row["source_indexes"]
            .as_array_mut()?
            .retain(|id| visible.iter().any(|s| s["index"] == *id));
    }
    let summary = json!({"schema":"les.model-coverage-summary.v1","sections":rows.len(),
        "partial":rows.iter().filter(|r|r["status"]=="partial").count(),
        "unknown":rows.iter().filter(|r|r["status"]=="unknown").count(),
        "scope":detail["scope"]});
    [detail, summary]
        .into_iter()
        .map(|v| object(COVERAGE_ID, v))
        .find(|item| tokens(&object_render(item)) < spare)
}
