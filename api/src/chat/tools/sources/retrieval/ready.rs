//! One body may have several independently pinned origins. Never merge origins.
//! This operates on ready evidence, after reading and source construction.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn requested() -> Result<bool> {
    match std::env::var("LES_EXPERIMENT_OVERVIEW_ADMISSION").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("") | Ok("baseline-v1") => Ok(false),
        Ok("ready-v1") => Ok(true),
        _ => Err("EVIDENCE_ADMISSION_EXPERIMENT_INVALID".into()),
    }
}

#[derive(Clone)]
struct Origin {
    value: Value,
    document: String,
    address: String,
}
impl Origin {
    fn new(value: Value) -> Result<Self> {
        for key in ["doc_id", "dataset_id", "doc_name"] {
            if !value[key].as_str().is_some_and(|s| !s.is_empty()) {
                return Err("EVIDENCE_PACKET_SOURCE_INVALID".into());
            }
        }
        if !value["source_sha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("EVIDENCE_PACKET_SOURCE_INVALID".into());
        }
        let document =
            json!([value["dataset_id"], value["doc_id"], value["source_sha256"]]).to_string();
        // Different occurrences in one document must not become one fact.
        let address = json!([
            value["page"],
            value["ordinal"],
            value["read_offset"],
            value["sheet_name"],
            value["row"],
            value["model_locator"],
            value["source_ref"]
        ])
        .to_string();
        Ok(Self {
            value,
            document,
            address,
        })
    }
}

struct Body {
    passage: Value,
    origins: Vec<Origin>,
    key: String,
}
impl Body {
    fn new(mut passage: Value, source: &Value) -> Result<Self> {
        if passage["source"] != *source || !passage["text"].is_string() {
            return Err("EVIDENCE_PACKET_SOURCE_INVALID".into());
        }
        let mut primary = source.clone();
        primary
            .as_object_mut()
            .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?
            .remove("equivalent_sources");
        // Preserve the per-origin passage locator when the shared body keeps
        // only its first origin's legacy top-level locator.
        if !passage["chunk_ord"].is_null() {
            primary["chunk_ord"] = passage["chunk_ord"].clone();
        }
        let mut origins = vec![Origin::new(primary)?];
        for alias in source["equivalent_sources"]
            .as_array()
            .into_iter()
            .flatten()
        {
            origins.push(Origin::new(alias.clone())?);
        }
        let mut content = passage
            .as_object()
            .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?
            .clone();
        for field in ["source", "source_ref", "chunk_ord"] {
            content.remove(field);
        }
        let mut context = source
            .as_object()
            .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?
            .clone();
        // Remove only identity, physical addressing and extraction metadata.
        // Unknown fields, headings, sheet meaning and warnings remain in the key.
        for field in [
            "doc_id",
            "dataset_id",
            "doc_name",
            "source_sha256",
            "url",
            "qdrant_point_id",
            "parent_id",
            "source_ref",
            "page",
            "source_page",
            "ordinal",
            "read_offset",
            "start_byte",
            "end_byte",
            "offset",
            "next_offset",
            "view_sha256",
            "reading_unit_sha256",
            "reading_unit_schema",
            "chunk_ord",
            "equivalent_sources",
            "evidence_kind",
            "reading_representation",
            "reading_origin",
            "extractor",
            "ocr_engine",
            "ocr_dpi",
            "page_text_complete",
            "context_unit",
            "document_fragment_count",
            "context_origin",
        ] {
            context.remove(field);
        }
        context.retain(|_, value| !value.is_null());
        if context.get("ocr") == Some(&json!(false)) {
            context.remove("ocr");
        }
        let key = les_core::tool_execution::canonical_json(&json!([content, context]))?;
        // Sources are reconstructed once from the complete, validated origins.
        passage.as_object_mut().unwrap().remove("source");
        Ok(Self {
            passage,
            origins,
            key,
        })
    }
    fn compatible(&self, other: &Self) -> bool {
        self.origins.iter().all(|a| {
            other
                .origins
                .iter()
                .all(|b| a.document != b.document || a.address == b.address)
        })
    }
    fn merge(&mut self, other: Self) {
        let mut seen: BTreeSet<_> = self.origins.iter().map(|o| o.value.to_string()).collect();
        for mut origin in other.origins {
            // Sharing text does not establish complete-document coverage.
            for field in ["context_unit", "document_fragment_count", "context_origin"] {
                origin.value.as_object_mut().unwrap().remove(field);
            }
            if seen.insert(origin.value.to_string()) {
                self.origins.push(origin);
            }
        }
    }
    fn finish(mut self) -> Value {
        let mut source = self.origins.remove(0).value;
        source["prepared_evidence_origin"] = json!(true);
        if !self.origins.is_empty() {
            source["shared_text_origin"] = json!(true);
            source["equivalent_sources"] = json!(self
                .origins
                .into_iter()
                .map(|mut o| {
                    o.value["shared_text_origin"] = json!(true);
                    o.value["prepared_evidence_origin"] = json!(true);
                    o.value
                })
                .collect::<Vec<_>>());
        }
        self.passage["source"] = source;
        self.passage
    }
}

pub(super) fn document_count(reply: &Value) -> usize {
    reply["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| {
            std::iter::once(s).chain(s["equivalent_sources"].as_array().into_iter().flatten())
        })
        .map(|s| json!([s["dataset_id"], s["doc_id"], s["source_sha256"]]).to_string())
        .collect::<BTreeSet<_>>()
        .len()
}

pub(super) fn prepare(reply: &mut Value) -> Result<()> {
    let passages = reply["result"]["passages"]
        .as_array()
        .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
    let sources = reply["sources"]
        .as_array()
        .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
    let ids = reply["result"]["read_receipt"]["point_ids"]
        .as_array()
        .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
    if passages.len() != sources.len() || passages.len() != ids.len() {
        return Err("EVIDENCE_PACKET_SOURCE_INVALID".into());
    }
    let input = passages.len();
    let mut bodies: Vec<Body> = Vec::new();
    let mut keys = BTreeMap::<String, Vec<usize>>::new();
    for (i, (passage, source)) in passages.iter().zip(sources).enumerate() {
        if source["qdrant_point_id"] != ids[i] || ids[i].is_null() {
            return Err("EVIDENCE_PACKET_SOURCE_INVALID".into());
        }
        let body = Body::new(passage.clone(), source)?;
        let existing = keys
            .get(&body.key)
            .and_then(|group| group.iter().copied().find(|&i| bodies[i].compatible(&body)));
        if let Some(i) = existing {
            bodies[i].merge(body);
        } else {
            keys.entry(body.key.clone()).or_default().push(bodies.len());
            bodies.push(body);
        }
    }
    let passages = bodies.into_iter().map(Body::finish).collect::<Vec<_>>();
    let sources = passages
        .iter()
        .map(|p| p["source"].clone())
        .collect::<Vec<_>>();
    reply["result"]["ready_evidence"] = json!({"schema":"les.ready-evidence.v1",
        "input_units":input,"unique_bodies":passages.len(),"shared_bodies":input-passages.len(),
        "identity_semantics":"shared_text_not_shared_edition_or_object"});
    reply["result"]["read_receipt"]["point_ids"] = json!(sources
        .iter()
        .map(|s| s["qdrant_point_id"].clone())
        .collect::<Vec<_>>());
    reply["sources"] = json!(sources);
    reply["result"]["included_fragments"] = json!(passages.len());
    reply["result"]["passages"] = json!(passages);
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub(super) fn origin(id: &str, digest: char, page: u64) -> Value {
        json!({"doc_id":id,"dataset_id":"scope","doc_name":format!("{id}.pdf"),
            "source_sha256":digest.to_string().repeat(64),"page":page,
            "source_ref":format!("{id}.pdf#page={page}"),"url":format!("/original/{id}#page={page}"),
            "qdrant_point_id":format!("point-{id}"),"section_heading":"Equipment ratings"})
    }
    pub(super) fn reply(rows: Vec<(&str, Value)>) -> Value {
        let passages = rows
            .iter()
            .enumerate()
            .map(|(n, (text, s))| {
                json!({"text":text,
            "canonical":null,"source":s,"source_ref":s["source_ref"],"chunk_ord":n})
            })
            .collect::<Vec<_>>();
        let sources = rows.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();
        json!({"tool":"search_sources","status":"ok","sources":sources,"result":{
            "passages":passages,"included_fragments":rows.len(),"coverage":"bounded",
            "overview":{"schema":"les.overview-coverage.v2","complete_corpus":false},
            "read_receipt":{"point_ids":sources.iter().map(|s|s["qdrant_point_id"].clone()).collect::<Vec<_>>()}}})
    }
    #[test]
    fn identical_ready_text_shares_body_not_provenance_or_extraction_claims() {
        let mut a = origin("a", 'a', 7);
        a["evidence_kind"] = json!("physical_pdf_page");
        a["reading_representation"] = json!("physical_pdf_page");
        a["ocr"] = json!(false);
        let b = origin("b", 'b', 1);
        let mut value = reply(vec![
            ("Exact rating 17.25 kW", a.clone()),
            ("Exact rating 17.25 kW", b.clone()),
        ]);
        prepare(&mut value).unwrap();
        assert_eq!(value["result"]["passages"].as_array().unwrap().len(), 1);
        let source = &value["sources"][0];
        assert_eq!(source["page"], 7);
        let alias = &source["equivalent_sources"][0];
        for field in [
            "doc_id",
            "dataset_id",
            "doc_name",
            "source_sha256",
            "page",
            "url",
            "source_ref",
            "qdrant_point_id",
        ] {
            assert_eq!(alias[field], b[field]);
        }
        assert!(alias["evidence_kind"].is_null());
        assert_eq!(alias["chunk_ord"], 1);
        assert_eq!(alias["shared_text_origin"], true);
        assert_eq!(
            value["result"]["read_receipt"]["point_ids"],
            json!(["point-a"])
        );
        let encoded = super::super::super::packet::encode(&value).unwrap();
        assert_eq!(
            super::super::super::packet::decode(&encoded).unwrap(),
            value
        );
    }
    #[test]
    fn same_text_with_different_meaning_or_distinct_occurrence_is_not_folded() {
        for kind in [
            "heading",
            "sheet",
            "number",
            "formula",
            "occurrence",
            "warning",
        ] {
            let a = origin("a", 'a', 1);
            let mut b = origin("b", 'b', 1);
            if kind == "heading" {
                b["section_heading"] = json!("Calculated instead of installed capacity");
            }
            if kind == "sheet" {
                b["sheet_name"] = json!("Different operating mode");
            }
            if kind == "warning" {
                b["retrieval_incomplete"] = json!(true);
            }
            if kind == "occurrence" {
                b = a.clone();
                b["page"] = json!(2);
                b["qdrant_point_id"] = json!("another-occurrence");
            }
            let mut value = reply(vec![
                ("17.25 kW", a),
                (
                    if kind == "number" {
                        "117.25 kW"
                    } else {
                        "17.25 kW"
                    },
                    b,
                ),
            ]);
            if kind == "formula" {
                value["result"]["passages"][0]["canonical"] =
                    json!({"value":17.25,"formula":"=A1+B1"});
                value["result"]["passages"][1]["canonical"] =
                    json!({"value":17.25,"formula":"=C1"});
            }
            prepare(&mut value).unwrap();
            assert_eq!(
                value["result"]["passages"].as_array().unwrap().len(),
                2,
                "{kind}"
            );
        }
    }
    #[test]
    fn mismatched_source_arrays_or_point_ids_fail_before_admission() {
        let base = reply(vec![("Data", origin("a", 'a', 1))]);
        let mut bad = base.clone();
        bad["sources"] = json!([]);
        assert!(prepare(&mut bad).is_err());
        let mut bad = base.clone();
        bad["result"]["read_receipt"]["point_ids"][0] = json!("wrong");
        assert!(prepare(&mut bad).is_err());
        let mut bad = base;
        bad["sources"][0]["source_sha256"] = json!("missing");
        bad["result"]["passages"][0]["source"] = bad["sources"][0].clone();
        assert!(prepare(&mut bad).is_err());
    }
}
