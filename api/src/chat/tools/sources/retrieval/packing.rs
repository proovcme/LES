//! Bounded parent expansion without starving later ranked sources.
use serde_json::Value;
use std::collections::HashSet;

fn unit(point: &Value, rank: usize) -> Value {
    let p = &point["payload"];
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
    serde_json::json!({"text":p["text"].as_str().unwrap_or(""),"unit_id":key(point),
        "source":source,"rank":if p["retrieval_expansion"]==true{Value::Null}else{serde_json::json!(p["retrieval_rank"].as_u64().unwrap_or(rank as u64))},"self_contained":matches!(p["reading_representation"].as_str(),Some("pdf_cells"|"word_structure"|"physical_pdf_page")),"heading":format!("{} {}",p["section_heading"].as_str().unwrap_or(""),p["parent_heading"].as_str().unwrap_or("")),"group":serde_json::json!([p["dataset_id"],p["file_name"],p["source_sha256"]]).to_string(),"point":point})
}
pub(super) fn catalog(points: &[Value]) -> Result<Value, &'static str> {
    les_core::context_selection::catalog(
        &serde_json::json!({"units":points.iter().enumerate().map(|(i,p)|unit(p,i)).collect::<Vec<_>>()}),
    )
}
/// A single core selector owns query relevance and duplicate content for both
/// the pre-hydration shortlist and the final context. No index score is changed.
fn select_points(
    points: &[Value],
    query: &str,
    text_budget: usize,
    context_budget: Option<usize>,
    preserve_rank: bool,
) -> Result<Vec<Value>, &'static str> {
    let input: Vec<_> = points.iter().enumerate().map(|(i, p)| unit(p, i)).collect();
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
            if preserve_rank {
                p["payload"]["retrieval_rank"] = u["rank"].clone();
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
    select_query_inner(
        seeds,
        sections,
        documents,
        budget,
        context_budget,
        query,
        false,
    )
}

/// Plan original I/O with the same selector and budget as final context. The
/// complete candidate catalog remains available; unplanned locators are not proof.
pub(super) fn read_plan(
    seeds: &[Value],
    mut hydrated: Value,
    budget: usize,
    context_budget: Option<usize>,
    query: &str,
) -> Result<(Vec<Value>, Value), &'static str> {
    let (planned, _) = select_query_inner(
        seeds,
        &hydrated["sections"],
        &hydrated["documents"],
        budget,
        context_budget,
        query,
        true,
    )?;
    let rows: std::collections::HashMap<_, _> =
        planned.iter().map(|p| (key(p), p.clone())).collect();
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array_mut().into_iter().flatten() {
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
    preserve_rank: bool,
) -> Result<(Vec<Value>, usize), &'static str> {
    let mut all: HashSet<String> = seeds.iter().map(key).collect();
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
    let selected = select_points(&pool, query, budget, context_budget, preserve_rank)?;
    Ok((selected, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn point(id: &str, parent: &str, text: &str, ord: u64) -> Value {
        json!({"id":id,"payload":{"dataset_id":"ds","file_name":parent,"parent_id":parent,"text":text,"chunk_ord":ord}})
    }
    #[test]
    fn original_read_plan_uses_common_budget_and_preserves_preselection_rank() {
        let heavy = point("large", "calculation.docx", &"unrelated ".repeat(100), 0);
        let needed = point("needed", "basis.docx", "supply category requirement", 1);
        let hydrated =
            json!({"sections":[{"points":[heavy.clone(), needed.clone()]}],"documents":[]});
        let (plan, trimmed) =
            read_plan(&[heavy, needed], hydrated, 80, None, "supply category").unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0]["id"], "needed");
        assert_eq!(plan[0]["payload"]["retrieval_rank"], 1);
        assert_eq!(trimmed["sections"][0]["points"].as_array().unwrap(), &plan);
        assert!(plan.iter().map(chars).sum::<usize>() <= 80);
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
        let (selected, total) = select(&[first, other], &Value::Null, &documents, 20, None);
        assert_eq!(selected.iter().map(key).collect::<Vec<_>>(), ["p1", "p3"]);
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
