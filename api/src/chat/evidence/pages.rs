//! Reassemble contiguous transfers of one physical page before interpreting text.
//! Text stays in Evidence; frames retain only identities and checked char ranges.
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Pages(BTreeMap<String, Frame>);
struct Frame {
    primary: Value,
    parts: Vec<Value>,
    end: usize,
    total: usize,
    extractor: String,
}
fn group(source: &Value) -> Option<String> {
    let id = source["doc_id"].as_str().filter(|s| !s.is_empty())?;
    let sha = source["source_sha256"].as_str()?;
    let page = source["page"].as_u64().filter(|n| *n > 0)?;
    if source["evidence_kind"] != "physical_pdf_page"
        || sha.len() != 64
        || !sha.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    Some(json!([id, source["dataset_id"], sha, page]).to_string())
}
impl Pages {
    pub(super) fn canonical(&self, source: &Value) -> Option<&Value> {
        let frame = self.0.get(&group(source)?)?;
        frame
            .parts
            .contains(&super::Evidence::key(source))
            .then_some(&frame.primary)
    }
    pub(super) fn normalize(
        &mut self,
        raw: &Value,
        prefix: impl FnOnce(&Value) -> Option<String>,
    ) -> Option<Value> {
        if raw["tool"] != "read_pdf_source" || raw["status"] != "ok" {
            return None;
        }
        let sources = raw["sources"].as_array()?;
        if sources.len() != 1 {
            return None;
        }
        let source = &sources[0];
        let key = group(source)?;
        let result = &raw["result"];
        if result["selection"] != "page_range" {
            self.0.remove(&key); // Literal windows have different coordinates.
            return None;
        }
        if source["source_sha256"] != result["source_sha256"]
            || source["page"] != result["page"]
            || source["offset"] != result["offset"]
        {
            return None;
        }
        let start = usize::try_from(result["offset"].as_u64()?).ok()?;
        let total = usize::try_from(result["total_chars"].as_u64()?).ok()?;
        let text = result["text"].as_str().filter(|s| !s.is_empty())?;
        let end = start.checked_add(text.chars().count())?;
        let next = result["next_offset"].as_u64();
        if end > total
            || if end < total {
                next != Some(end as u64)
            } else {
                !result["next_offset"].is_null()
            }
        {
            return None;
        }
        let extractor = result["extractor"].as_str().filter(|s| !s.is_empty())?;
        let identity = super::Evidence::key(source);
        if start == 0 {
            self.0.insert(
                key,
                Frame {
                    primary: identity.clone(),
                    parts: vec![identity],
                    end,
                    total,
                    extractor: extractor.into(),
                },
            );
            return None;
        }
        let frame = self.0.get_mut(&key)?;
        if frame.end != start || frame.total != total || frame.extractor != extractor {
            return None; // Never bridge gaps, overlaps, different pages or revisions.
        }
        let mut text_before = prefix(&frame.primary)?;
        if text_before.chars().count() != start {
            return None;
        }
        text_before.push_str(text); // No separator inside a word or table cell.
        frame.end = end;
        frame.parts.push(identity);
        let mut assembled = raw.clone();
        assembled["result"]["text"] = json!(text_before);
        assembled["result"]["offset"] = json!(0);
        assembled["result"]["complete"] = json!(end == total);
        assembled["result"]["assembled_part_count"] = json!(frame.parts.len());
        assembled["sources"][0]["offset"] = json!(0);
        Some(assembled)
    }
}
