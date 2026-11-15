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
    Ok(
        serde_json::json!({"text":literal.as_ref().and_then(|u|u["text"].as_str()).unwrap_or(p["text"].as_str().unwrap_or("")),"unit_id":key(point),
        "read_identity":serde_json::json!([p["dataset_id"],p["file_name"],p["source_sha256"],address]),
        "source":source,"source_rank":p["retrieval_source_rank"],"rank":if p["retrieval_expansion"]==true{Value::Null}else{serde_json::json!(rank)},"self_contained":matches!(p["reading_representation"].as_str(),Some("pdf_cells"|"word_structure"|"physical_pdf_page")),"heading":format!("{} {}",p["section_heading"].as_str().unwrap_or(""),p["parent_heading"].as_str().unwrap_or("")),"group":serde_json::json!([p["dataset_id"],p["file_name"],p["source_sha256"]]).to_string(),"point":point}),
    )
}
pub(super) fn catalog(points: &[Value]) -> Result<Value, &'static str> {
    les_core::context_selection::catalog(
        &serde_json::json!({"units":points.iter().enumerate().map(|(i,p)|unit(p,i)).collect::<Result<Vec<_>,_>>()?}),
    )
}
/// The final context alone owns semantic selection and content deduplication.
fn select_points(
    points: &[Value],
    query: &str,
    text_budget: usize,
    context_budget: Option<usize>,
) -> Result<Vec<Value>, &'static str> {
    let input = points
        .iter()
        .enumerate()
        .map(|(i, p)| unit(p, i))
        .collect::<Result<Vec<_>, _>>()?;
    let result = les_core::context_selection::select(&serde_json::json!({
        "question":query,"units":input,"max_chars":context_budget.unwrap_or(1_000_000),
        "text_budget":text_budget,"identity_reserve_chars":if context_budget.is_some(){1024}else{0},"max_items":32
    }))?;
    Ok(result["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let mut p = u["point"].clone();
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

// One budget owner for documents, parents and individual hits. Admission is
// atomic, deduplicated, and counts source identities as well as text.
struct Window {
    selected: Vec<Value>,
    seen: HashSet<String>,
    text_left: usize,
    context_left: usize,
    overhead: usize,
}
impl Window {
    fn new(text: usize, context: Option<usize>) -> Self {
        Self {
            selected: vec![],
            seen: HashSet::new(),
            text_left: text,
            context_left: context.unwrap_or(usize::MAX),
            overhead: if context.is_some() { 1024 } else { 0 },
        }
    }
    fn admit(&mut self, rows: &[Value]) -> bool {
        let mut unique = HashSet::new();
        let rows: Vec<_> = rows
            .iter()
            .filter(|p| !self.seen.contains(&key(p)) && unique.insert(key(p)))
            .collect();
        let cost: usize = rows.iter().map(|r| chars(r)).sum();
        let context_cost = cost + rows.len() * self.overhead;
        if cost > self.text_left
            || context_cost > self.context_left
            || self.selected.len() + rows.len() > 32
        {
            return false;
        }
        self.text_left -= cost;
        self.context_left -= context_cost;
        for row in rows {
            self.seen.insert(key(row));
            self.selected.push(row.clone());
        }
        true
    }
}

/// Explicit pages admit a contiguous prefix of the frozen reading stream.
/// Neighbors must not consume cursor positions or bypass a non-fitting unit.
pub(super) fn prefix(rows: &[Value], budget: usize, context: Option<usize>) -> Vec<Value> {
    let mut window = Window::new(budget, context);
    for row in rows {
        if !window.admit(std::slice::from_ref(row)) {
            break;
        }
    }
    window.selected
}

#[cfg(test)]
pub(super) fn select(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
    context_budget: Option<usize>,
) -> (Vec<Value>, usize) {
    select_query(seeds, sections, documents, budget, context_budget, "").unwrap()
}
pub(super) fn select_query(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
    context_budget: Option<usize>,
    query: &str,
) -> Result<(Vec<Value>, usize), &'static str> {
    select_query_inner(seeds, sections, documents, budget, context_budget, query)
}

/// Bound original I/O by anchors/sources. The size of an indexed parent must
/// not discard a source before its original has yielded small readable units.
pub(super) fn read_plan(
    seeds: &[Value],
    mut hydrated: Value,
    _budget: usize,
    _context_budget: Option<usize>,
    _query: &str,
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

fn select_query_inner(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
    context_budget: Option<usize>,
    query: &str,
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
    let selected = select_points(&pool, query, budget, context_budget)?;
    Ok((selected, total))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            1,
            Some(1),
            "unrelated query",
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
            1,
            Some(1),
            "",
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
    fn read_plan_is_query_independent_and_does_not_spend_the_answer_budget() {
        let mut a = point("a", "a.pdf", &"x".repeat(1_100_000), 0);
        a["payload"]["retrieval_rank"] = json!(2);
        let mut b = point("b", "b.pdf", "tiny", 0);
        b["payload"]["retrieval_rank"] = json!(1);
        let seeds = vec![a, b];
        let first = read_plan(
            &seeds,
            json!({"sections":[],"documents":[]}),
            1,
            Some(1),
            "tiny",
        )
        .unwrap();
        let other = read_plan(
            &seeds,
            json!({"sections":[],"documents":[]}),
            100000,
            Some(100000),
            "different",
        )
        .unwrap();
        assert_eq!(first, other);
        assert_eq!(first.0.len(), 2);
        assert_eq!(first.0[0]["id"], "b");
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
            10000,
            None,
            "",
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
    fn original_read_plan_defers_text_budget_until_after_original_read() {
        let heavy = point("large", "calculation.docx", &"unrelated ".repeat(100), 0);
        let needed = point("needed", "basis.docx", "supply category requirement", 1);
        let hydrated =
            json!({"sections":[{"points":[heavy.clone(), needed.clone()]}],"documents":[]});
        let (plan, trimmed) =
            read_plan(&[heavy, needed], hydrated, 80, None, "supply category").unwrap();
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
        let (selected, _) = select_query(
            &plan,
            &Value::Null,
            &Value::Null,
            80,
            None,
            "supply category",
        )
        .unwrap();
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
        let (selected, total) = select(&[first, later], &Value::Null, &documents, 20, None);
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
            None,
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
        let (selected, _) = select(&[first, later], &Value::Null, &documents, 20, None);
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
            None,
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
        let (selected, total) = select(&[a, b.clone()], &sections, &Value::Null, 20, None);
        assert_eq!(selected, vec![b]);
        assert_eq!(total, 2);
    }
    #[test]
    fn many_small_rows_must_reserve_space_for_their_source_identities() {
        let seeds: Vec<_> = (0..16)
            .map(|i| point(&i.to_string(), "sheet", &format!("x{i}"), i))
            .collect();
        let (selected, total) = select(
            &seeds,
            &serde_json::json!([]),
            &Value::Null,
            16000,
            Some(3500),
        );
        assert_eq!(total, 16);
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
