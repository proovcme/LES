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
    fit_budget(reply, max_chars, max_items, None, false)
}

pub(super) fn fit_compact_context(reply: &mut Value) -> Result<()> {
    fit_budget(reply, 22500, 240, None, true)
}

pub(super) fn fit_overview_context(reply: &mut Value, text_budget: usize) -> Result<()> {
    if super::ready::requested()? {
        return fit_ready_overview(reply, text_budget);
    }
    reply["result"]["context_policy"] = json!("document-coverage-v1");
    let documents = |value: &Value| {
        value["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| json!([s["dataset_id"], s["doc_id"], s["source_sha256"]]).to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    let readable = documents(reply);
    // Reserve the largest counters before the sole wire admission. Final
    // counter updates only shrink that representation, never evict evidence.
    reply["result"]["overview"]["readable_primary_documents"] = json!(readable);
    reply["result"]["overview"]["delivered_primary_documents"] = json!(readable);
    reply["result"]["overview"]["omitted_primary_documents"] = json!(readable);
    fit_budget(reply, 22500, 240, Some(text_budget), true)?;
    let delivered = documents(reply);
    reply["result"]["overview"]["delivered_primary_documents"] = json!(delivered);
    reply["result"]["overview"]["omitted_primary_documents"] =
        json!(readable.saturating_sub(delivered));
    Ok(())
}

fn fit_ready_overview(reply: &mut Value, text_budget: usize) -> Result<()> {
    let readable = super::ready::document_count(reply);
    super::ready::prepare(reply)?;
    if let Some(overview) = reply["result"]["overview"].as_object_mut() {
        for key in [
            "readable_primary_documents",
            "delivered_primary_documents",
            "omitted_primary_documents",
        ] {
            overview.remove(key);
        }
    }
    reply["result"]["overview"]["schema"] = json!("les.overview-coverage.v3");
    reply["result"]["overview"]["coverage_unit"] = json!("independently_pinned_document_origins");
    reply["result"]["overview"]["readable_documents"] = json!(readable);
    reply["result"]["overview"]["delivered_documents"] = json!(readable);
    reply["result"]["overview"]["omitted_documents"] = json!(readable);
    // Sole admission uses the actual compact serialization of the ready bodies
    // and all their origins, never the old indexed/physical completeness flag.
    fit_budget(reply, 22500, 240, Some(text_budget), true)?;
    let delivered = super::ready::document_count(reply);
    reply["result"]["overview"]["delivered_documents"] = json!(delivered);
    reply["result"]["overview"]["omitted_documents"] = json!(readable.saturating_sub(delivered));
    Ok(())
}

#[cfg(test)]
#[test]
fn one_ready_body_leaves_room_for_a_later_independent_source() {
    use super::ready::tests::{origin, reply};
    let repeated = "A".repeat(5000);
    let independent = "B".repeat(3000);
    let input = reply(vec![
        (repeated.as_str(), origin("a", 'a', 7)),
        (repeated.as_str(), origin("b", 'b', 1)),
        (independent.as_str(), origin("c", 'c', 2)),
    ]);
    let mut baseline = input.clone();
    fit_budget(&mut baseline, 22500, 240, Some(12000), true).unwrap();
    assert!(!baseline["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["doc_id"] == "c"));
    let mut candidate = input;
    fit_ready_overview(&mut candidate, 12000).unwrap();
    assert!(candidate["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["doc_id"] == "c"));
    assert_eq!(candidate["result"]["overview"]["delivered_documents"], 3);
    assert_eq!(candidate["result"]["overview"]["omitted_documents"], 0);
    assert!(canonical_chars(&super::super::packet::encode(&candidate).unwrap()) <= 22500);
    assert_eq!(candidate["result"]["passages"].as_array().unwrap().len(), 2);
}

#[cfg(test)]
#[test]
fn wire_pressure_cannot_strip_the_origins_of_a_ready_body() {
    use super::ready::tests::{origin, reply};
    let mut rows = vec![
        ("Shared evidence".to_owned(), origin("a", 'a', 7)),
        ("Shared evidence".to_owned(), origin("b", 'b', 1)),
    ];
    for i in 0..40 {
        rows.push((
            format!("Independent {i}: {}", "x".repeat(1000)),
            origin(&format!("other-{i}"), 'c', 1),
        ));
    }
    let mut value = reply(
        rows.iter()
            .map(|(text, source)| (text.as_str(), source.clone()))
            .collect(),
    );
    assert!(canonical_chars(&super::super::packet::encode(&value).unwrap()) > 22500);
    fit_ready_overview(&mut value, 12000).unwrap();
    let source = &value["sources"][0];
    assert_eq!(source["doc_id"], "a");
    assert_eq!(source["equivalent_sources"][0]["doc_id"], "b");
    assert_eq!(
        source["equivalent_sources"][0]["source_sha256"],
        "b".repeat(64)
    );
    assert!(source["equivalent_sources_omitted"].is_null());
    assert!(canonical_chars(&super::super::packet::encode(&value).unwrap()) <= 22500);
}

/// A context snapshot has no continuation cursor. Its only admission pass
/// measures ready records, retaining RRF order among the units which fit.
pub(super) fn fit_ranked_context(reply: &mut Value, text_budget: usize) -> Result<()> {
    reply["result"]["context_policy"] = json!("rrf-wire-v1");
    fit_budget(reply, 22500, 240, Some(text_budget), false)
}

fn fit_budget(
    reply: &mut Value,
    max_chars: usize,
    max_items: usize,
    text_budget: Option<usize>,
    compact: bool,
) -> Result<()> {
    if compact {
        super::super::packet::encode(reply)?;
    }
    let wire_chars = |value: &Value| {
        if compact {
            super::super::packet::encode(value)
                .map(|v| canonical_chars(&v))
                .unwrap_or(usize::MAX)
        } else {
            canonical_chars(value)
        }
    };
    let wire_items = |value: &Value| {
        if compact {
            super::super::packet::encode(value)
                .map(|v| json_item_count(&v))
                .unwrap_or(usize::MAX)
        } else {
            json_item_count(value)
        }
    };
    let ranked = text_budget.is_some();
    let text_budget = text_budget.unwrap_or(usize::MAX);
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
    let planned_units = orig_passages.len();
    let planned_text_chars: usize = orig_passages
        .iter()
        .map(|p| p["text"].as_str().map_or(0, |s| s.chars().count()))
        .sum();
    reconcile_document_units(reply, &orig_passages);
    if wire_chars(reply) <= max_chars
        && wire_items(reply) <= max_items
        && (!ranked || (planned_text_chars <= text_budget && planned_units <= 32))
    {
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
    if wire_chars(reply) > max_chars || wire_items(reply) > max_items {
        compact_read_failures(reply);
    }
    // Source identities occur both beside each passage and in the public
    // source map. Remove only repeated aliases inside each identity before
    // spending the wire allowance on fewer original reading units.
    if wire_chars(reply) > max_chars || wire_items(reply) > max_items {
        for source in reply["sources"].as_array_mut().into_iter().flatten() {
            compact_repeated_identity(source);
        }
        for passage in reply["result"]["passages"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            compact_repeated_identity(&mut passage["source"]);
        }
    }
    // Exact duplicate origins are navigation, not extra read evidence. Keep
    // every primary pinned source; omit alias detail explicitly if it would
    // consume the body budget. Different text/revisions remain separate units.
    if !ranked && (wire_chars(reply) > max_chars || wire_items(reply) > max_items) {
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
    let mut admitted_text = 0usize;
    let mut skipped_text = 0usize;
    let mut skipped_wire = 0usize;
    let mut skipped_count = 0usize;
    for i in 0..orig_passages.len() {
        let chars = orig_passages[i]["text"]
            .as_str()
            .map_or(0, |s| s.chars().count());
        if ranked && (kept_passages.len() >= 32 || admitted_text + chars > text_budget) {
            if kept_passages.len() >= 32 {
                skipped_count += 1;
            } else {
                skipped_text += 1;
            }
            omitted_any = true;
            continue;
        }
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
        if !ranked {
            reconcile_document_units(reply, &orig_passages);
        }
        if ranked && (wire_chars(reply) > max_chars || wire_items(reply) > max_items) {
            // Unselected candidates must not erase aliases of fitting units.
            // Retry only this ready candidate without optional alias details.
            compact_aliases(&mut kept_passages.last_mut().unwrap()["source"]);
            compact_aliases(kept_sources.last_mut().unwrap());
            reply["result"]["passages"] = json!(kept_passages);
            reply["sources"] = json!(kept_sources);
        }
        if wire_chars(reply) > max_chars || wire_items(reply) > max_items {
            kept_passages.pop();
            kept_sources.pop();
            kept_ids.pop();
            omitted_any = true;
            skipped_wire += 1;
            // Only ranked context snapshots can skip a whole unit. Explicit
            // source streams must stop here, or their cursor would skip text.
            if !ranked {
                break;
            }
        } else {
            admitted_text += chars;
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
            if wire_chars(reply) > max_chars || wire_items(reply) > max_items {
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
            if wire_chars(reply) > max_chars || wire_items(reply) > max_items {
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
            if wire_chars(reply) > max_chars || wire_items(reply) > max_items {
                kept.pop();
                reply["result"]["fusion_trace"] = json!(kept);
                reply["result"]["fusion_trace_truncated"] = json!(true);
                break;
            }
        }
        reply["result"]["fusion_trace_truncated"] = json!(kept.len() != total);
    }
    eprintln!(
        "{}",
        json!({"event":"source_envelope_admission","tool":reply["tool"],
            "planned_units":planned_units,"delivered_units":kept_passages.len(),
            "planned_text_chars":planned_text_chars,
            "delivered_text_chars":kept_passages.iter().map(|p|p["text"].as_str().map_or(0,|s|s.chars().count())).sum::<usize>(),
            "wire_chars":wire_chars(reply),"wire_limit_chars":max_chars,
            "body_omitted":omitted_any,"ranked_context":ranked,
            "skipped_text_budget":skipped_text,"skipped_wire_budget":skipped_wire,
            "skipped_unit_limit":skipped_count})
    );
    Ok(())
}

fn compact_aliases(source: &mut Value) {
    // Ready-body origins are mandatory evidence, not optional navigation.
    // Admit the whole body with its pins, or skip it; never silently turn a
    // shared text body back into a claim supported by only its first file.
    if source["prepared_evidence_origin"] == true {
        return;
    }
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

// These are optional aliases for values that remain directly available. A
// locator carrying any additional geometry/identity is never folded away.
// Strong IDs, revision hashes, cursors, canonical cells and warnings remain.
pub(super) fn compact_repeated_identity(source: &mut Value) {
    let compatible_page =
        source["source_page"].is_null() || source["source_page"] == source["page"];
    let redundant_locator = source["locator"].as_object().is_some_and(|locator| {
        compatible_page
            && locator
                .keys()
                .all(|key| matches!(key.as_str(), "source_sha256" | "source_page"))
            && locator.get("source_sha256") == source.get("source_sha256")
            && locator.get("source_page") == source.get("page")
    });
    let repeated_page = source.get("source_page") == source.get("page");
    let repeated_heading = source.get("parent_heading") == source.get("section_heading");
    if let Some(object) = source.as_object_mut() {
        if redundant_locator {
            object.remove("locator");
        }
        if repeated_page {
            object.remove("source_page");
        }
        if repeated_heading {
            object.remove("parent_heading");
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
        let mut source = Value::Object(original.clone());
        compact_repeated_identity(&mut source);
        source.as_object_mut().unwrap().remove("url");
        if let Some(name) = original.get("doc_name").and_then(Value::as_str) {
            source.as_object_mut().unwrap().insert(
                "doc_name".into(),
                json!(name.rsplit(['/', '\\']).next().unwrap_or(name)),
            );
        }
        failure["source"] = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_cost_keeps_more_selected_proof_without_reordering_or_losing_sources() {
        let sources: Vec<_> = (0..6).map(|page|json!({"doc_id":"d","dataset_id":"ds","doc_name":"folder/".repeat(120),"source_sha256":"a".repeat(64),"page":page+1,"url":format!("/original#page={}",page+1)})).collect();
        let rows:Vec<_>=sources.iter().enumerate().map(|(i,s)|json!({"text":format!("Position {i}: 17.25 kW {}","proof ".repeat(140)),"canonical":{"kind":"paragraph"},"source":s})).collect();
        let original = json!({"result":{"passages":rows,"coverage":"bounded"},"sources":sources});
        let mut baseline = original.clone();
        fit_envelope_budget(&mut baseline, 22500, 240).unwrap();
        let mut candidate = original.clone();
        fit_compact_context(&mut candidate).unwrap();
        let packed = super::super::super::packet::encode(&candidate).unwrap();
        assert!(canonical_chars(&packed) <= 22500);
        assert!(
            candidate["result"]["passages"].as_array().unwrap().len()
                >= baseline["result"]["passages"].as_array().unwrap().len()
        );
        assert_eq!(
            super::super::super::packet::decode(&packed).unwrap(),
            candidate
        );
        for (i, p) in candidate["result"]["passages"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(p, &original["result"]["passages"][i]);
        }
    }
    #[test]
    fn a_rejected_candidate_cannot_remove_a_fitting_units_source_aliases() {
        let alias = json!({"doc_id":"copy","dataset_id":"scope","source_sha256":"b".repeat(64)});
        let source = json!({"doc_id":"primary","dataset_id":"scope","source_sha256":"a".repeat(64),"equivalent_sources":[alias]});
        let other = json!({"doc_id":"other","dataset_id":"scope","source_sha256":"c".repeat(64)});
        let mut reply = json!({"sources":[source,other],"result":{"passages":[
            {"text":"Exact value 17.25","source":source}, {"text":"x".repeat(30000),"source":other}]}});
        fit_ranked_context(&mut reply, 12000).unwrap();
        assert_eq!(reply["result"]["passages"].as_array().unwrap().len(), 1);
        assert_eq!(reply["sources"][0]["equivalent_sources"][0], alias);
        assert_eq!(
            reply["result"]["passages"][0]["source"]["equivalent_sources"][0],
            alias
        );
    }
    #[test]
    fn ranked_context_skips_an_oversized_unit_while_a_stream_stops_before_it() {
        let source = |n| json!({"doc_id":format!("d-{n}"),"dataset_id":"scope","source_sha256":"a".repeat(64),"qdrant_point_id":format!("p-{n}")});
        let a = source(0);
        let b = source(1);
        let c = source(2);
        let mut context = json!({"sources":[a,b,c],"result":{"passages":[
            {"text":"First exact value 17.25","source":a},
            {"text":"x".repeat(30000),"source":b},
            {"text":"Last exact value 117.250 東京","source":c}],
            "read_receipt":{"point_ids":["p-0","p-1","p-2"]}}});
        let mut stream = context.clone();
        fit_ranked_context(&mut context, 12000).unwrap();
        assert_eq!(context["result"]["passages"].as_array().unwrap().len(), 2);
        assert_eq!(
            context["result"]["passages"][1]["text"],
            "Last exact value 117.250 東京"
        );
        assert_eq!(
            context["result"]["read_receipt"]["point_ids"],
            json!(["p-0", "p-2"])
        );
        assert_eq!(context["sources"][1], c);
        assert!(canonical_chars(&context) <= 22500);
        fit_envelope_budget(&mut stream, 22500, 240).unwrap();
        assert_eq!(stream["result"]["passages"].as_array().unwrap().len(), 1);
        assert_eq!(
            stream["result"]["read_receipt"]["point_ids"],
            json!(["p-0"])
        );
    }
    #[test]
    fn ready_table_and_source_costs_cannot_hide_a_text_budget_overflow() {
        let source = json!({"doc_id":"a","dataset_id":"scope","source_sha256":"a".repeat(64)});
        let mut context = json!({"sources":[source,source,source],"result":{"passages":[
            {"text":"17.25","canonical":{"schema":"les.document-structure.v1","kind":"table","rows":[["cell".repeat(10000)]]},"source":source},
            {"text":"117.250","source":source},
            {"text":"An unrelated longer body","source":source}]}});
        fit_ranked_context(&mut context, 7).unwrap();
        assert_eq!(context["result"]["passages"].as_array().unwrap().len(), 1);
        assert_eq!(context["result"]["passages"][0]["text"], "117.250");
        assert!(canonical_chars(&context) <= 22500);
    }
    #[test]
    fn repeated_source_metadata_cannot_evict_read_bodies() {
        let sources = (1..=2)
            .map(|page| {
                json!({"doc_id":"original","dataset_id":"scope","source_sha256":"a".repeat(64),
                "page":page,"source_page":page,"qdrant_point_id":format!("anchor-{page}"),
                "view_sha256":"b".repeat(64),"offset":0,"next_offset":null,
                "url":format!("/original?source_sha256={}#page={page}","a".repeat(64)),
                "locator":{"source_sha256":"a".repeat(64),"source_page":page},
                "section_heading":"Heading ".repeat(64),"parent_heading":"Heading ".repeat(64)})
            })
            .collect::<Vec<_>>();
        let texts = [
            format!("First controlled value 17.25. {}", "proof ".repeat(80)),
            format!("Second controlled value 117.250. {}", "proof ".repeat(80)),
        ];
        let mut reply = json!({"sources":sources,"result":{"passages":[
            {"source":sources[0],"text":texts[0]}, {"source":sources[1],"text":texts[1]}]}});
        assert!(canonical_chars(&reply) > 5000);
        fit_envelope_budget(&mut reply, 5000, 240).unwrap();
        assert_eq!(reply["result"]["passages"].as_array().unwrap().len(), 2);
        for (i, text) in texts.iter().enumerate() {
            assert_eq!(reply["result"]["passages"][i]["text"], *text);
            for source in [
                &reply["sources"][i],
                &reply["result"]["passages"][i]["source"],
            ] {
                for field in [
                    "doc_id",
                    "dataset_id",
                    "source_sha256",
                    "page",
                    "qdrant_point_id",
                    "view_sha256",
                    "offset",
                    "next_offset",
                    "url",
                    "section_heading",
                ] {
                    assert_eq!(source[field], sources[i][field]);
                }
                assert!(source.get("locator").is_none() && source.get("source_page").is_none());
                assert!(source.get("parent_heading").is_none());
            }
        }
        assert!(canonical_chars(&reply) <= 5000);
    }
    #[test]
    fn distinct_geometry_pages_and_parent_headings_are_not_redundant_aliases() {
        let mut source = json!({"source_sha256":"a".repeat(64),"page":1,"source_page":2,
            "locator":{"source_sha256":"a".repeat(64),"source_page":1,"bbox":[1,2,3,4]},
            "section_heading":"Child clause","parent_heading":"Parent chapter",
            "table_projection_warning":"PDF_TABLE_SOURCE_MISMATCH","ocr_numbers_verified":false});
        let original = source.clone();
        compact_repeated_identity(&mut source);
        assert_eq!(source, original);
        source["locator"] = json!({"source_sha256":"a".repeat(64),"source_page":1});
        let distinct_page = source.clone();
        compact_repeated_identity(&mut source);
        assert_eq!(source, distinct_page);
        source["locator"] = json!({"source_sha256":"c".repeat(64),"source_page":1});
        let changed = source.clone();
        compact_repeated_identity(&mut source);
        assert_eq!(source, changed);
    }
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
