//! Search and reading representations share the same indexed page identity.
//! The searchable text/vector is never replaced or re-ranked by this projection.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
fn key(point: &Value) -> String {
    let p = &point["payload"];
    json!([
        p["dataset_id"],
        p["file_name"],
        p["source_sha256"],
        p["page"]
    ])
    .to_string()
}
pub(super) fn project(
    seeds: Vec<Value>,
    mut hydrated: Value,
) -> Result<(Vec<Value>, Value), &'static str> {
    let mut views = HashMap::new();
    for group in ["sections", "documents"] {
        for row in hydrated[group]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|s| s["points"].as_array().into_iter().flatten())
        {
            let p = &row["payload"];
            if let Some(text) = p["pdf_page_view"].as_str() {
                if p["pdf_page_view_schema"] != "les.pdf-page-view.v1"
                    || !p["page"].is_u64()
                    || text.chars().count() > 16000
                    || p["pdf_page_view_sha256"] != les_core::sha256_hex(text.as_bytes())
                {
                    return Err("PDF_PAGE_VIEW_INVALID");
                }
                let mut page = row.clone();
                page["payload"]["text"] = json!(text);
                page["payload"]["reading_representation"] = json!("pdf_cells");
                if let Some(previous) = views.insert(key(row), page.clone()) {
                    if previous != page {
                        return Err("PDF_PAGE_VIEW_INVALID");
                    }
                }
            }
        }
    }
    let project = |rows: &[Value]| {
        let mut seen = HashSet::new();
        rows.iter()
            .map(|row| {
                let mut projected = views.get(&key(row)).unwrap_or(row).clone();
                if row["payload"]["equivalent_points"].is_array() {
                    projected["payload"]["equivalent_points"] =
                        row["payload"]["equivalent_points"].clone();
                }
                projected
            })
            .filter(|row| seen.insert(row["id"].to_string()))
            .collect::<Vec<_>>()
    };
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array_mut().into_iter().flatten() {
            section["points"] = json!(project(
                section["points"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            ));
        }
    }
    Ok((project(&seeds), hydrated))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(id: &str, dataset: &str, view: bool) -> Value {
        let mut p = json!({"id":id,"payload":{"dataset_id":dataset,"file_name":"a.pdf","source_sha256":"s","page":1,"text":"Search text"}});
        if view {
            p["payload"]["pdf_page_view"] = json!("|Item|Unit|Qty|\n|A|m|17.25|");
            p["payload"]["pdf_page_view_sha256"] = json!(les_core::sha256_hex(
                p["payload"]["pdf_page_view"].as_str().unwrap().as_bytes()
            ));
            p["payload"]["pdf_page_view_schema"] = json!("les.pdf-page-view.v1");
        }
        p
    }
    #[test]
    fn reading_projection_deduplicates_a_page_without_changing_search_or_crossing_scope() {
        let first = page("first", "a", true);
        let tail = page("tail", "a", false);
        let other = page("other", "b", false);
        let raw = json!({"sections":[{"points":[first,tail]}],"documents":[]});
        let before = raw.clone();
        let (seeds, view) = project(vec![tail, other.clone()], raw.clone()).unwrap();
        assert_eq!(raw, before);
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0]["id"], "first");
        assert_eq!(seeds[1], other);
        assert_eq!(view["sections"][0]["points"].as_array().unwrap().len(), 1);
        assert_eq!(
            before["sections"][0]["points"][0]["payload"]["text"],
            "Search text"
        );
    }
    #[test]
    fn a_corrupt_reading_view_is_not_admitted_as_source_text() {
        let mut p = page("first", "a", true);
        p["payload"]["pdf_page_view_sha256"] = json!("bad");
        assert_eq!(
            project(
                vec![p.clone()],
                json!({"sections":[{"points":[p]}],"documents":[]})
            )
            .unwrap_err(),
            "PDF_PAGE_VIEW_INVALID"
        );
    }
}
