//! Bounded parent expansion without starving later ranked sources.
use serde_json::Value;
use std::collections::HashSet;

pub(super) fn fused_rank(point: &Value, rank: usize) -> u64 {
    let p = &point["payload"];
    // A reserved retriever head may have entered a small candidate pool from
    // far below its boundary. Its admission position is not its fused rank.
    p["retrieval_rank"]
        .as_u64()
        .or_else(|| {
            point["retrieval_fusion"]["fusion_rank"]
                .as_u64()
                .map(|n| n.saturating_sub(1))
        })
        .unwrap_or(rank as u64)
}
fn unit(point: &Value, rank: usize) -> Result<Value, &'static str> {
    let p = &point["payload"];
    let rank = fused_rank(point, rank);
    let mut source = serde_json::Map::new();
    for field in [
        "dataset_id",
        "file_name",
        "source_sha256",
        "page",
        "source_page",
        "parent_id",
        "source_ref",
        "model_locator",
        "sheet_name",
        "row",
    ] {
        if !p[field].is_null() {
            source.insert(field.into(), p[field].clone());
        }
    }
    if p["pdf_excerpt_line"].is_u64() {
        source.insert("qdrant_point_id".into(), p["retrieval_anchor_id"].clone());
    }
    let address = if p["source_unit_id"].is_u64() {
        serde_json::json!(["word", p["source_unit_id"]])
    } else if p["page"].is_u64() {
        serde_json::json!(["page", p["page"]])
    } else {
        serde_json::json!(["parent", p["parent_id"]])
    };
    let literal = les_core::document_search_unit(
        &serde_json::json!({"canonical":p["canonical"],"kind":if p["canonical"]["kind"]=="table"{"table_rows"}else{"paragraph"},"heading":p["section_heading"]}),
    )?;
    let heading = if p["pdf_excerpt_line"].is_u64() {
        String::new()
    } else {
        format!(
            "{} {}",
            p["section_heading"].as_str().unwrap_or(""),
            p["parent_heading"].as_str().unwrap_or("")
        )
    };
    let has_rank = p["retrieval_expansion"] != true
        && !(p["pdf_excerpt_line"].is_u64()
            && super::super::pdf_focus::bare_code(p["text"].as_str().unwrap_or("")));
    Ok(
        serde_json::json!({"text":literal.as_ref().and_then(|u|u["text"].as_str()).unwrap_or(p["text"].as_str().unwrap_or("")),"unit_id":key(point),
        "read_identity":serde_json::json!([p["dataset_id"],p["file_name"],p["source_sha256"],address]),
        "source":source,"source_rank":p["retrieval_source_rank"],"rank":if has_rank{serde_json::json!(rank)}else{Value::Null},"self_contained":matches!(p["reading_representation"].as_str(),Some("pdf_cells"|"pdf_excerpt"|"word_structure"|"physical_pdf_page")),"heading":heading,"group":serde_json::json!([p["dataset_id"],p["file_name"],p["source_sha256"]]).to_string(),"point":point}),
    )
}
/// Rank and deduplicate hydrated evidence. Internal prefetch leaves room
/// calculations to the final model context.
fn select_points(
    points: &[Value],
    query: &str,
    text_budget: usize,
    control: super::trial::Control,
    model_pack: bool,
) -> Result<Vec<Value>, &'static str> {
    control.snapshot("readable_pool", query, points);
    let input = points
        .iter()
        .enumerate()
        .map(|(i, p)| unit(p, i))
        .collect::<Result<Vec<_>, _>>()?;
    let request = serde_json::json!({
        "question":query,"units":input,"max_chars":1_000_000,
        "text_budget":text_budget,"max_items":if model_pack {64} else {32},
        "policy":match control {super::trial::Control::RrfWire=>"rrf-order-v1",super::trial::Control::DocumentCoverage=>"document-coverage-v1",_=>"coverage-mmr-v1"}
    });
    let result = if model_pack {
        les_core::context_selection::rank_for_model(&request)
    } else {
        les_core::context_selection::select(&request)
    }?;
    Ok(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let mut p = u["point"].clone();
            if model_pack && u["prefer_fragments"] == true {
                p["prefer_fragments"] = serde_json::json!(true);
            }
            let mut aliases = p["payload"]["equivalent_points"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            aliases.extend(
                u["equivalent_sources"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            if !aliases.is_empty() {
                p["payload"]["equivalent_points"] = serde_json::json!(aliases);
            }
            p
        })
        .collect())
}

fn key(point: &Value) -> String {
    point["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| point["id"].to_string())
}
fn chars(point: &Value) -> usize {
    point["payload"]["text"]
        .as_str()
        .unwrap_or("")
        .chars()
        .count()
}
fn same_parent(point: &Value, scope: &Value) -> bool {
    ["dataset_id", "file_name", "parent_id"]
        .iter()
        .all(|field| point["payload"][*field] == scope[*field])
}

/// Explicit pages admit a contiguous prefix of the frozen reading stream.
/// Neighbors must not consume cursor positions or bypass a non-fitting unit.
pub(super) fn prefix(rows: &[Value], mut text_left: usize, context: Option<usize>) -> Vec<Value> {
    let identity_reserve = if context.is_some() { 1024 } else { 0 };
    let mut context_left = context.unwrap_or(usize::MAX);
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        if !seen.insert(key(row)) {
            continue;
        }
        let text = chars(row);
        let cost = text + identity_reserve;
        if selected.len() == 32 || text > text_left || cost > context_left {
            break;
        }
        text_left -= text;
        context_left -= cost;
        selected.push(row.clone());
    }
    selected
}

#[cfg(test)]
pub(super) fn select(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
) -> (Vec<Value>, usize) {
    select_query(seeds, sections, documents, budget, "").unwrap()
}
#[cfg(test)]
pub(super) fn select_query(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
    query: &str,
) -> Result<(Vec<Value>, usize), &'static str> {
    controlled_query(
        seeds,
        sections,
        documents,
        budget,
        query,
        super::trial::Control::Off,
        false,
    )
}

/// Bound original I/O by anchors/sources. The size of an indexed parent must
/// not discard a source before its original has yielded small readable units.
pub(super) fn read_plan(
    seeds: &[Value],
    mut hydrated: Value,
) -> Result<(Vec<Value>, Value), &'static str> {
    // This is an I/O bound, not a second retriever. Indexed wording, parent
    // length and lexical similarity must not reject an original before reading.
    let mut ranked = seeds
        .iter()
        .enumerate()
        .map(|(i, p)| (fused_rank(p, i), p))
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, _)| *rank);
    let mut pool = ranked
        .into_iter()
        .map(|(rank, p)| {
            let mut p = p.clone();
            p["payload"]["retrieval_rank"] = serde_json::json!(rank);
            p
        })
        .collect::<Vec<_>>();
    let original_ids: HashSet<_> = seeds.iter().map(key).collect();
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array().into_iter().flatten() {
            for point in section["points"].as_array().into_iter().flatten() {
                if !original_ids.contains(&key(point)) {
                    let mut point = point.clone();
                    point["payload"]["retrieval_expansion"] = serde_json::json!(true);
                    point["payload"]["retrieval_rank"] = Value::Null;
                    pool.push(point);
                }
            }
        }
    }
    // Bound physical reads, not arbitrary index point IDs. A page may have
    // several hits; a bounded hydrated document includes continuation pages
    // that did not independently win a retrieval vote.
    let scope = |point: &Value| {
        let p = &point["payload"];
        serde_json::json!([
            p["dataset_id"],
            p["file_name"],
            p["source_sha256"],
            p["page"]
        ])
        .to_string()
    };
    // Every ranked hit belongs to the bounded retrieval shortlist (at most 64
    // anchors at the caller). Reading must not silently impose a smaller
    // second shortlist: otherwise a late independent source can never reach
    // final context selection. Optional parent pages use only the remaining
    // room in the existing 32-scope expansion window.
    let required_scopes: HashSet<_> = seeds.iter().map(scope).collect();
    let max_read_scopes = 32.max(required_scopes.len());
    let mut scopes = HashSet::new();
    let mut seen = HashSet::new();
    let planned = pool
        .into_iter()
        .filter(|p| {
            if !seen.insert(key(p)) {
                return false;
            }
            let address = scope(p);
            if scopes.contains(&address) {
                return true;
            }
            scopes.len() < max_read_scopes && scopes.insert(address)
        })
        .collect::<Vec<_>>();
    let rows: std::collections::HashMap<_, _> =
        planned.iter().map(|p| (key(p), p.clone())).collect();
    let planned_originals = original_ids
        .iter()
        .filter(|id| rows.contains_key(*id))
        .count();
    hydrated["read_plan"] = serde_json::json!({"input_anchors":original_ids.len(),"planned_units":planned.len(),"planned_read_scopes":scopes.len(),"required_read_scopes":required_scopes.len(),"omitted_original_anchors":original_ids.len()-planned_originals,"max_read_scopes":max_read_scopes,"context_budget_applied_after_original_read":true});
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array_mut().into_iter().flatten() {
            section["read_plan_original_units"] =
                serde_json::json!(section["points"].as_array().map_or(0, Vec::len));
            if let Some(points) = section["points"].as_array_mut() {
                *points = points
                    .iter()
                    .filter_map(|p| rows.get(&key(p)).cloned())
                    .collect();
            }
        }
    }
    Ok((planned, hydrated))
}

pub(super) fn controlled_query(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
    query: &str,
    control: super::trial::Control,
    model_pack: bool,
) -> Result<(Vec<Value>, usize), &'static str> {
    let retrieved: HashSet<String> = seeds.iter().map(key).collect();
    let mut all = retrieved.clone();
    let mut groups = Vec::new();
    for section in sections.as_array().into_iter().flatten() {
        let mut rows = section["points"].as_array().cloned().unwrap_or_default();
        rows.sort_by_key(|row| {
            let ord = row["payload"]["chunk_ord"].as_u64().unwrap_or(0);
            let distance = seeds
                .iter()
                .filter(|seed| same_parent(seed, &section["scope"]))
                .map(|seed| ord.abs_diff(seed["payload"]["chunk_ord"].as_u64().unwrap_or(0)))
                .min()
                .unwrap_or(u64::MAX);
            (distance, ord)
        });
        for row in &rows {
            all.insert(key(row));
        }
        groups.push(rows);
    }
    for document in documents.as_array().into_iter().flatten() {
        for row in document["points"].as_array().into_iter().flatten() {
            all.insert(key(row));
        }
    }
    let total = all.len();
    // Consider the complete hydrated pool before admission. Earlier sources do
    // not spend the budget before a lower-ranked complementary unit is scored.
    let mut pool = seeds.to_vec();
    for rows in &groups {
        pool.extend(rows.iter().cloned());
    }
    for document in documents.as_array().into_iter().flatten() {
        pool.extend(document["points"].as_array().into_iter().flatten().cloned());
    }
    for point in &mut pool {
        if !retrieved.contains(&key(point)) {
            // Neighbor/parent reads are additional evidence, not additional
            // retrieval votes. Their position in this materialized pool has
            // no dense/BM25 rank, even if a stale transient field was supplied.
            point["payload"]["retrieval_expansion"] = serde_json::json!(true);
            point["payload"]["retrieval_rank"] = Value::Null;
        }
    }
    if control != super::trial::Control::Off {
        // Both trial arms start from one stable pool. Hash-map iteration and
        // duplicate-primary ties must not masquerade as an admission effect.
        pool.sort_by_key(|p| {
            let payload = &p["payload"];
            let expansion = payload["retrieval_expansion"] == true;
            let prior = if expansion {
                payload["retrieval_source_rank"].as_u64()
            } else {
                payload["retrieval_rank"].as_u64()
            };
            (expansion, prior.unwrap_or(u64::MAX), key(p))
        });
        let mut seen = HashSet::new();
        pool.retain(|p| seen.insert(key(p)));
    }
    let selected = select_points(&pool, query, budget, control, model_pack)?;
    control.snapshot("selection", query, &selected);
    Ok((selected, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_arms_do_not_depend_on_materialized_container_iteration_order() {
        let mut a = point("a", "one", "query first proof", 0);
        let mut b = point("b", "two", "query other proof", 0);
        a["payload"]["retrieval_rank"] = json!(0);
        b["payload"]["retrieval_rank"] = json!(0);
        for control in [
            super::super::trial::Control::Observe,
            super::super::trial::Control::RrfWire,
        ] {
            let left = controlled_query(
                &[a.clone(), b.clone()],
                &Value::Null,
                &Value::Null,
                100,
                "query",
                control,
                false,
            )
            .unwrap();
            let right = controlled_query(
                &[b.clone(), a.clone()],
                &Value::Null,
                &Value::Null,
                100,
                "query",
                control,
                false,
            )
            .unwrap();
            assert_eq!(left, right);
        }
    }
    #[test]
    fn every_retrieved_scope_is_read_before_optional_parent_expansion() {
        let mut seeds = (0..64)
            .map(|n| {
                let mut p = point(&format!("hit-{n}"), &format!("source-{n}.pdf"), "text", n);
                p["payload"]["page"] = json!(1);
                p
            })
            .collect::<Vec<_>>();
        // Reserved retriever heads retain their true late fused rank, but
        // remain required reads. A small answer window cannot delete them.
        seeds[63]["payload"]["retrieval_rank"] = json!(800);
        let optional = point("parent", "optional.pdf", "parent text", 0);
        let (plan, hydrated) = read_plan(
            &seeds,
            json!({"sections":[{"points":[optional]}],"documents":[]}),
        )
        .unwrap();
        assert_eq!(plan.len(), 64);
        assert_eq!(
            plan.iter().map(key).collect::<HashSet<_>>(),
            seeds.iter().map(key).collect::<HashSet<_>>()
        );
        assert_eq!(hydrated["read_plan"]["required_read_scopes"], 64);
        assert_eq!(hydrated["read_plan"]["max_read_scopes"], 64);
        assert_eq!(hydrated["read_plan"]["omitted_original_anchors"], 0);
        assert!(hydrated["sections"][0]["points"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(plan.last().unwrap()["payload"]["retrieval_rank"], 800);
    }
    #[test]
    fn document_continuations_survive_read_planning_without_retriever_votes() {
        let mut rows = (0..40)
            .map(|n| {
                let mut p = point(&format!("p{n}"), "document.pdf", "body", n);
                p["payload"]["page"] = serde_json::json!(n + 1);
                p
            })
            .collect::<Vec<_>>();
        let seed = rows[0].clone();
        // Several points on one physical page must consume just one read.
        let mut caption = rows[1].clone();
        caption["id"] = serde_json::json!("caption");
        rows.insert(2, caption);
        let (plan, hydrated) = read_plan(
            &[seed.clone()],
            serde_json::json!({"documents":[{"points":rows}],"sections":[]}),
        )
        .unwrap();
        assert_eq!(plan.len(), 33);
        assert_eq!(hydrated["read_plan"]["planned_read_scopes"], 32);
        assert_eq!(hydrated["documents"][0]["read_plan_original_units"], 41);
        assert_eq!(plan[0]["id"], seed["id"]);
        assert!(plan[1..]
            .iter()
            .all(|p| p["payload"]["retrieval_expansion"] == true
                && p["payload"]["retrieval_rank"].is_null()));
    }
    #[test]
    fn read_plan_uses_rank_even_when_reading_units_differ_in_size() {
        let mut a = point("a", "a.pdf", &"x".repeat(1_100_000), 0);
        a["payload"]["retrieval_rank"] = json!(2);
        let mut b = point("b", "b.pdf", "tiny", 0);
        b["payload"]["retrieval_rank"] = json!(1);
        let seeds = vec![a, b];
        let (plan, _) = read_plan(&seeds, json!({"sections":[],"documents":[]})).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0]["id"], "b");
    }
    use serde_json::json;
    #[test]
    fn unread_parent_neighbors_cannot_invent_a_retrieved_rank() {
        let seed = point("hit", "document", "retrieved passage", 0);
        let mut neighbor = point("neighbor", "document", "unrelated neighboring text", 1);
        neighbor["payload"]["retrieval_rank"] = json!(0);
        let (plan, _) = read_plan(
            &[seed.clone()],
            json!({"sections":[{"points":[seed.clone(),neighbor]}],"documents":[]}),
        )
        .unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0]["id"], seed["id"]);
        assert_eq!(plan[1]["payload"]["retrieval_expansion"], true);
        assert!(plan[1]["payload"]["retrieval_rank"].is_null());
    }
    #[test]
    fn retaining_a_channel_head_does_not_reset_its_actual_fusion_rank() {
        let mut point = serde_json::json!({"id":"reserved", "payload":{"text":"candidate"},
            "retrieval_fusion":{"fusion_rank":80,"selection_rank":4}});
        assert_eq!(unit(&point, 3).unwrap()["rank"], 79);
        point["payload"]["retrieval_rank"] = serde_json::json!(79);
        assert_eq!(unit(&point, 0).unwrap()["rank"], 79);
        point["payload"]["retrieval_expansion"] = serde_json::json!(true);
        assert!(unit(&point, 0).unwrap()["rank"].is_null());
    }
    fn point(id: &str, parent: &str, text: &str, ord: u64) -> Value {
        json!({"id":id,"payload":{"dataset_id":"ds","file_name":parent,"parent_id":parent,"text":text,"chunk_ord":ord}})
    }
    #[test]
    fn final_selection_follows_the_search_query_instead_of_the_overall_topic() {
        let seeds = [
            point("fact", "a", "Расчётная мощность ВРУ 84 кВт.", 0),
            point("topic", "b", "Общая справка по электроснабжению здания.", 1),
        ];
        let (selected, _) = select_query(
            &seeds,
            &Value::Null,
            &Value::Null,
            50,
            "расчётная мощность ВРУ",
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["id"], "fact");
    }
    #[test]
    fn original_read_plan_defers_text_budget_until_after_original_read() {
        let heavy = point("large", "calculation.docx", &"unrelated ".repeat(100), 0);
        let needed = point("needed", "basis.docx", "supply category requirement", 1);
        let hydrated =
            json!({"sections":[{"points":[heavy.clone(), needed.clone()]}],"documents":[]});
        let (plan, trimmed) = read_plan(&[heavy, needed], hydrated).unwrap();
        assert_eq!(plan.len(), 2);
        let needed = plan.iter().find(|p| p["id"] == "needed").unwrap();
        assert_eq!(needed["payload"]["retrieval_rank"], 1);
        assert!(plan
            .iter()
            .any(|p| p["id"] == "large" && p["payload"]["retrieval_rank"] == 0));
        assert_eq!(
            trimmed["sections"][0]["points"]
                .as_array()
                .unwrap()
                .iter()
                .map(key)
                .collect::<HashSet<_>>(),
            plan.iter().map(key).collect::<HashSet<_>>()
        );
        let (selected, _) =
            select_query(&plan, &Value::Null, &Value::Null, 80, "supply category").unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["id"], "needed");
        assert!(selected.iter().map(chars).sum::<usize>() <= 80);
    }
    #[test]
    fn whole_first_document_cannot_evict_a_later_ranked_source() {
        let first = point("a", "first", "hit", 0);
        let continuation = point("a2", "first", &"x".repeat(15), 1);
        let later = point("b", "second", "other hit", 0);
        let documents = json!([{"scope":first["payload"],"points":[first,continuation]}]);
        let (selected, total) = select(&[first, later], &Value::Null, &documents, 20);
        assert_eq!(selected.iter().map(key).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(total, 3);
        assert!(selected.iter().map(chars).sum::<usize>() <= 20);
    }
    #[test]
    fn document_continuation_precedes_lower_ranked_unrelated_hits() {
        let first = point("p1", "document", "hit", 0);
        let tail = point("p3", "document", "continuation", 2);
        let other = point("b", "other", &"x".repeat(18), 0);
        let documents = json!([{"scope":first["payload"],"points":[first,tail]}]);
        // The continuation is relevant to the question. With an empty query,
        // merely belonging to the file must not invent a retrieval vote.
        let (selected, total) = select_query(
            &[first, other],
            &Value::Null,
            &documents,
            20,
            "hit continuation",
        )
        .unwrap();
        assert_eq!(selected.iter().map(key).collect::<Vec<_>>(), ["p1", "p3"]);
        assert_eq!(selected[1]["payload"]["retrieval_expansion"], true);
        assert_eq!(total, 3);
    }
    #[test]
    fn a_lower_ranked_fitting_document_does_not_replace_a_higher_ranked_large_hit() {
        let first = point("a", "large", "ranked proof", 0);
        let later = point("b", "small", &"x".repeat(16), 0);
        let documents = json!([{"scope":later["payload"],"points":[later]}]);
        let (selected, _) = select(&[first, later], &Value::Null, &documents, 20);
        assert_eq!(selected.iter().map(key).collect::<Vec<_>>(), ["a"]);
    }
    #[test]
    fn a_small_later_parent_keeps_its_answer_despite_a_large_first_parent() {
        let first = point("a", "large", "hit", 5);
        let later = point("b", "small", "heading", 0);
        let tail = point("c", "small", "actual answer", 1);
        let sections = json!([
            {"scope":first["payload"],"points":[first,point("d","large",&"x".repeat(40),6),point("e","large",&"y".repeat(40),7)]},
            {"scope":later["payload"],"points":[later,tail]}
        ]);
        let (selected, total) = select_query(
            &[first, later],
            &sections,
            &Value::Null,
            55,
            "actual answer",
        )
        .unwrap();
        assert_eq!(total, 5);
        assert_eq!(
            selected
                .iter()
                .map(key)
                .collect::<std::collections::BTreeSet<_>>(),
            ["a", "b", "c"].into_iter().map(String::from).collect()
        );
        assert!(selected.iter().map(chars).sum::<usize>() <= 55);
    }
    #[test]
    fn ranked_matches_survive_and_oversized_fragments_are_never_truncated() {
        let a = point("a", "large", &"é".repeat(60), 0);
        let b = point("b", "small", "proof", 0);
        let sections =
            json!([{"scope":a["payload"],"points":[a]}, {"scope":b["payload"],"points":[b]}]);
        let (selected, total) = select(&[a, b.clone()], &sections, &Value::Null, 20);
        assert_eq!(selected, vec![b]);
        assert_eq!(total, 2);
    }
    #[test]
    fn many_small_rows_must_reserve_space_for_their_source_identities() {
        let seeds: Vec<_> = (0..16)
            .map(|i| point(&i.to_string(), "sheet", &format!("x{i}"), i))
            .collect();
        let selected = prefix(&seeds, 16000, Some(3500));
        assert_eq!(selected.len(), 3);
        assert!(selected.iter().map(chars).sum::<usize>() + selected.len() * 1024 <= 3500);
    }
    #[test]
    fn explicit_reading_keeps_identical_text_at_distinct_stream_positions() {
        let a = point("page-1", "document", "Identical page text", 0);
        let b = point("page-2", "document", "Identical page text", 1);
        assert_eq!(prefix(&[a.clone(), b.clone()], 1000, None), vec![a, b]);
    }
}
