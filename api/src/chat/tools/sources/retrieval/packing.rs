//! Bounded parent expansion without starving later ranked sources.
use serde_json::Value;
use std::collections::HashSet;

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

pub(super) fn select(
    seeds: &[Value],
    sections: &Value,
    documents: &Value,
    budget: usize,
    context_budget: Option<usize>,
) -> (Vec<Value>, usize) {
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
    let mut window = Window::new(budget, context_budget);
    // Protect ranked hits before spending the remaining budget on expansion.
    // A fitting complete document must not evict another retrieved source.
    for seed in seeds {
        window.admit(std::slice::from_ref(seed));
    }
    for seed in seeds {
        let document = documents.as_array().into_iter().flatten().find(|d| {
            ["dataset_id", "file_name"]
                .iter()
                .all(|f| seed["payload"][*f] == d["scope"][*f])
        });
        if let Some(rows) = document.and_then(|d| d["points"].as_array()) {
            window.admit(rows);
        }
    }
    // Complete fitting parents, then share partial-parent opportunities.
    for rows in &groups {
        window.admit(rows);
    }
    let depth = groups.iter().map(Vec::len).max().unwrap_or(0);
    for index in 0..depth {
        for rows in &groups {
            if let Some(row) = rows.get(index) {
                window.admit(std::slice::from_ref(row));
            }
        }
    }
    let selected = window.selected;
    (selected, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn point(id: &str, parent: &str, text: &str, ord: u64) -> Value {
        json!({"id":id,"payload":{"dataset_id":"ds","file_name":parent,"parent_id":parent,"text":text,"chunk_ord":ord}})
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
        let (selected, total) = select(&[first, later], &sections, &Value::Null, 55, None);
        assert_eq!(total, 5);
        assert_eq!(
            selected.iter().map(key).collect::<Vec<_>>(),
            ["a", "b", "c"]
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
            .map(|i| point(&i.to_string(), "sheet", "x", i))
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
}
