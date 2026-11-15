//! Whole-block wire admission; never owns source-stream pagination.
use super::*;
pub(super) fn json_item_count(value: &Value) -> usize {
    match value {
        Value::Object(map) => map.values().map(json_item_count).sum(),
        Value::Array(values) => values.len() + values.iter().map(json_item_count).sum::<usize>(),
        _ => 0,
    }
}

pub(super) fn canonical_chars(value: &Value) -> usize {
    les_core::tool_execution::canonical_json(value)
        .map(|s| s.chars().count())
        .unwrap_or_else(|_| value.to_string().chars().count())
}

fn reconcile_document_units(reply: &mut Value, orig_passages: &[Value]) {
    let mut orig_doc_counts = std::collections::HashMap::<(String, String), usize>::new();
    let mut orig_unit_counts = std::collections::HashMap::<String, usize>::new();
    for p in orig_passages {
        let s = &p["source"];
        let doc_key = (
            s["doc_id"].as_str().unwrap_or("").to_owned(),
            s["source_sha256"].as_str().unwrap_or("").to_owned(),
        );
        *orig_doc_counts.entry(doc_key).or_default() += 1;
        if let Some(unit) = s["context_unit"].as_str().filter(|u| !u.is_empty()) {
            *orig_unit_counts.entry(unit.to_owned()).or_default() += 1;
        }
    }
    let mut kept_doc_counts = std::collections::HashMap::<(String, String), usize>::new();
    let mut kept_unit_counts = std::collections::HashMap::<String, usize>::new();
    if let Some(passages) = reply["result"]["passages"].as_array() {
        for p in passages {
            let s = &p["source"];
            let doc_key = (
                s["doc_id"].as_str().unwrap_or("").to_owned(),
                s["source_sha256"].as_str().unwrap_or("").to_owned(),
            );
            *kept_doc_counts.entry(doc_key).or_default() += 1;
            if let Some(unit) = s["context_unit"].as_str().filter(|u| !u.is_empty()) {
                *kept_unit_counts.entry(unit.to_owned()).or_default() += 1;
            }
        }
    }
    let is_complete = |s: &Value| -> bool {
        let doc_key = (
            s["doc_id"].as_str().unwrap_or("").to_owned(),
            s["source_sha256"].as_str().unwrap_or("").to_owned(),
        );
        let kept_doc = kept_doc_counts.get(&doc_key).copied().unwrap_or(0);
        let orig_doc = orig_doc_counts.get(&doc_key).copied().unwrap_or(kept_doc);
        let expected_doc = s["document_fragment_count"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or(orig_doc);
        let unit_ok = s["context_unit"]
            .as_str()
            .filter(|u| !u.is_empty())
            .is_none_or(|u| {
                let kept_u = kept_unit_counts.get(u).copied().unwrap_or(0);
                let orig_u = orig_unit_counts.get(u).copied().unwrap_or(kept_u);
                kept_u == orig_u && kept_u == expected_doc
            });
        kept_doc > 0 && kept_doc == orig_doc && kept_doc == expected_doc && unit_ok
    };
    if let Some(passages) = reply["result"]["passages"].as_array_mut() {
        for p in passages {
            if !is_complete(&p["source"]) {
                if let Some(obj) = p["source"].as_object_mut() {
                    obj.remove("context_unit");
                    obj.remove("document_fragment_count");
                    obj.remove("context_origin");
                }
            }
        }
    }
    if let Some(sources) = reply["sources"].as_array_mut() {
        for s in sources {
            if !is_complete(s) {
                if let Some(obj) = s.as_object_mut() {
                    obj.remove("context_unit");
                    obj.remove("document_fragment_count");
                    obj.remove("context_origin");
                }
            }
        }
    }
}

pub(crate) fn fit_envelope_budget(
    reply: &mut Value,
    max_chars: usize,
    max_items: usize,
) -> Result<()> {
    // A canonical table carries nested cell arrays, unlike a flat Markdown
    // string. Keep the byte/character bound and atomic reading units; apply
    // the registered cell-item allowance only to that typed representation.
    let max_items = if reply["result"]["passages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|p| {
            p["canonical"]["schema"] == "les.document-structure.v1"
                && p["canonical"]["kind"] == "table"
        }) {
        8192
    } else {
        max_items
    };
    let orig_passages = reply["result"]["passages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    reconcile_document_units(reply, &orig_passages);
    if canonical_chars(reply) <= max_chars && json_item_count(reply) <= max_items {
        return Ok(());
    }
    // Debug ranks are optional transport data. They must never evict the
    // document body they describe. Reattach only what fits after evidence.
    let trace = reply["result"]
        .get("fusion_trace")
        .and_then(Value::as_array)
        .cloned();
    if let Some(rows) = &trace {
        reply["result"]["fusion_trace"] = json!([]);
        reply["result"]["fusion_trace_total"] = json!(rows.len());
        reply["result"]["fusion_trace_truncated"] = json!(!rows.is_empty());
    }
    if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
        compact_read_failures(reply);
    }
    // Exact duplicate origins are navigation, not extra read evidence. Keep
    // every primary pinned source; omit alias detail explicitly if it would
    // consume the body budget. Different text/revisions remain separate units.
    if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
        for source in reply["sources"].as_array_mut().into_iter().flatten() {
            compact_aliases(source);
        }
        for passage in reply["result"]["passages"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            compact_aliases(&mut passage["source"]);
        }
    }
    let orig_passages = reply["result"]["passages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let orig_sources = reply["sources"].as_array().cloned().unwrap_or_default();
    let orig_ids = reply["result"]["read_receipt"]["point_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let orig_candidates = reply["result"]
        .get("candidate_documents")
        .and_then(Value::as_array)
        .cloned();
    let orig_hints = reply["result"]
        .get("navigation_hints")
        .and_then(Value::as_array)
        .cloned();
    if orig_candidates.is_some() {
        reply["result"]["candidate_documents"] = json!([]);
    }
    if orig_hints.is_some() {
        reply["result"]["navigation_hints"] = json!([]);
    }
    let mut kept_passages = Vec::new();
    let mut kept_sources = Vec::new();
    let mut kept_ids = Vec::new();
    let mut omitted_any = false;
    for i in 0..orig_passages.len() {
        kept_passages.push(orig_passages[i].clone());
        kept_sources.push(
            orig_sources
                .get(i)
                .cloned()
                .unwrap_or_else(|| orig_passages[i]["source"].clone()),
        );
        kept_ids.push(
            orig_ids
                .get(i)
                .cloned()
                .unwrap_or_else(|| orig_passages[i]["source"]["qdrant_point_id"].clone()),
        );
        reply["result"]["passages"] = json!(kept_passages);
        reply["sources"] = json!(kept_sources);
        if reply["result"]["read_receipt"].is_object() {
            reply["result"]["read_receipt"]["point_ids"] = json!(kept_ids);
        }
        reply["result"]["included_fragments"] = json!(kept_passages.len());
        reply["result"]["coverage"] = json!("bounded");
        reconcile_document_units(reply, &orig_passages);
        if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
            kept_passages.pop();
            kept_sources.pop();
            kept_ids.pop();
            omitted_any = true;
            // Preserve contiguous block order so next_offset continues without gaps.
            break;
        }
    }
    reply["result"]["passages"] = json!(kept_passages);
    reply["sources"] = json!(kept_sources);
    if reply["result"]["read_receipt"].is_object() {
        reply["result"]["read_receipt"]["point_ids"] = json!(kept_ids);
    }
    reply["result"]["included_fragments"] = json!(kept_passages.len());
    reconcile_document_units(reply, &orig_passages);
    if omitted_any {
        reply["result"]["coverage"] = json!("bounded");
    }
    if kept_passages.is_empty() && !orig_passages.is_empty() {
        return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
    }
    if let Some(hints) = orig_hints {
        let mut kept_hints = Vec::new();
        for hint in hints {
            kept_hints.push(hint);
            reply["result"]["navigation_hints"] = json!(kept_hints);
            if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
                kept_hints.pop();
                reply["result"]["navigation_hints"] = json!(kept_hints);
                break;
            }
        }
    }
    if let Some(candidates) = orig_candidates {
        let cap = if omitted_any { 16 } else { 64 };
        let mut kept_candidates = Vec::new();
        for cand in candidates.into_iter().take(cap) {
            kept_candidates.push(cand);
            reply["result"]["candidate_documents"] = json!(kept_candidates);
            if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
                kept_candidates.pop();
                reply["result"]["candidate_documents"] = json!(kept_candidates);
                break;
            }
        }
    }
    if let Some(trace) = trace {
        let total = trace.len();
        let mut kept = Vec::new();
        for row in trace {
            kept.push(row);
            reply["result"]["fusion_trace"] = json!(kept);
            reply["result"]["fusion_trace_truncated"] = json!(kept.len() != total);
            if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
                kept.pop();
                reply["result"]["fusion_trace"] = json!(kept);
                reply["result"]["fusion_trace_truncated"] = json!(true);
                break;
            }
        }
        reply["result"]["fusion_trace_truncated"] = json!(kept.len() != total);
    }
    Ok(())
}

fn compact_aliases(source: &mut Value) {
    if let Some(aliases) = source
        .get_mut("equivalent_sources")
        .and_then(Value::as_array_mut)
    {
        let count = aliases.len();
        if count > 0 {
            aliases.clear();
            source["equivalent_sources_omitted"] = json!(count);
            source["provenance_coverage"] = json!("primary_only");
        }
    }
}

// A failed page is not evidence. Its code, readable name and exact retry
// identity must survive, but repeated paths, URLs and locator copies must not
// consume the successfully read bodies' wire allowance.
fn compact_read_failures(reply: &mut Value) {
    for failure in reply["result"]["read_failures"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        let Some(original) = failure["source"].as_object() else {
            continue;
        };
        let mut source = original.clone();
        source.remove("url");
        if original
            .get("locator")
            .and_then(Value::as_object)
            .is_some_and(|v| {
                v.keys()
                    .all(|k| matches!(k.as_str(), "source_sha256" | "source_page"))
                    && v.get("source_sha256") == original.get("source_sha256")
                    && v.get("source_page") == original.get("page")
            })
        {
            source.remove("locator");
        }
        if original.get("source_page") == original.get("page") {
            source.remove("source_page");
        }
        if let Some(name) = original.get("doc_name").and_then(Value::as_str) {
            source.insert(
                "doc_name".into(),
                json!(name.rsplit(['/', '\\']).next().unwrap_or(name)),
            );
        }
        failure["source"] = Value::Object(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_preserve_retry_identity_without_evicting_verified_bodies() {
        let source = json!({"doc_id":"proof","source_sha256":"a".repeat(64),"page":1});
        let failures = (1..=10).map(|page|json!({"code":"PDF_TABLE_SOURCE_MISMATCH",
            "message":"Original not read; this is not proof of absence.","evidence_admissible":false,"read_required":"read_pdf_source",
                "source":{"doc_id":"unread","dataset_id":"scope","source_sha256":"b".repeat(64),"page":page,"offset":17,
                "doc_name":format!("{}source.pdf","nested-directory/".repeat(30)),
                "url":format!("/raw/{}?sha256={}","source-directory/".repeat(30),"b".repeat(64)),
                "locator":{"source_sha256":"b".repeat(64),"source_page":page}}})).collect::<Vec<_>>();
        let passages = (0..3)
            .map(|n| json!({"source":source,"text":format!("Proof {n} {}","x".repeat(2990))}))
            .collect::<Vec<_>>();
        let mut reply = json!({"sources":vec![source;3],"result":{"read_failures":failures,"passages":passages}});
        assert!(canonical_chars(&reply) > 18000);
        fit_envelope_budget(&mut reply, 18000, 230).unwrap();
        assert_eq!(reply["result"]["passages"], json!(passages));
        assert!(canonical_chars(&reply) <= 18000);
        for (i, failure) in reply["result"]["read_failures"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(failure["code"], "PDF_TABLE_SOURCE_MISMATCH");
            assert_eq!(failure["source"]["doc_id"], "unread");
            assert_eq!(failure["source"]["source_sha256"], "b".repeat(64));
            assert_eq!(failure["source"]["page"], i + 1);
            assert_eq!(failure["source"]["offset"], 17);
            assert_eq!(failure["source"]["doc_name"], "source.pdf");
            assert_eq!(failure["evidence_admissible"], false);
            assert_eq!(failure["read_required"], "read_pdf_source");
            assert!(failure["message"]
                .as_str()
                .unwrap()
                .contains("not proof of absence"));
        }
    }
    #[test]
    fn typed_table_cell_arrays_fit_the_registered_wire_contract_atomically() {
        let rows = (0..100)
            .map(|n| json!([format!("Item {n}"), "17.25", "kg"]))
            .collect::<Vec<_>>();
        let mut reply = json!({"sources":[{}],"result":{"passages":[{"source":{},"text":"table view", "canonical":{
            "schema":"les.document-structure.v1","kind":"table","header":["Item","Qty","Unit"],"rows":rows}}]}});
        assert!(json_item_count(&reply) > 230);
        let original = reply["result"]["passages"][0].clone();
        fit_envelope_budget(&mut reply, 22000, 230).unwrap();
        assert_eq!(reply["result"]["passages"][0], original);
        assert!(canonical_chars(&reply) <= 22000 && json_item_count(&reply) <= 8192);
        assert!(fit_envelope_budget(&mut reply, 100, 230).is_err());
    }
    #[test]
    fn diagnostics_and_duplicate_aliases_never_evict_primary_source_body() {
        let source = json!({"doc_id":"primary","source_sha256":"a".repeat(64),
            "equivalent_sources":(0..20).map(|n|json!({"doc_id":n,"doc_name":"Long source path ".repeat(30)})).collect::<Vec<_>>()});
        let text = "|Item|Qty|\n|---|---|\n|Pump|17.25|\n".repeat(20);
        let mut reply = json!({"sources":[source.clone()],"result":{"passages":[{"text":text,"source":source}],
            "fusion_trace":(0..64).map(|n|json!({"id":n,"provenance":vec!["dense","lexical"]})).collect::<Vec<_>>()}});
        fit_envelope_budget(&mut reply, 2500, 100).unwrap();
        assert_eq!(reply["result"]["passages"][0]["text"], text);
        assert_eq!(reply["sources"][0]["doc_id"], "primary");
        assert_eq!(reply["sources"][0]["equivalent_sources_omitted"], 20);
        assert_eq!(reply["result"]["fusion_trace_truncated"], true);
        assert!(canonical_chars(&reply) <= 2500 && json_item_count(&reply) <= 100);
    }
}
