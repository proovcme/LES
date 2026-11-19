//! Retrieval anchors define a structural reading window, never a document-wide vote.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub(super) struct Anchor {
    pub index: usize,
    pub rank: u64,
}
pub(super) struct Plan {
    pub indices: BTreeSet<usize>,
    pub matches: BTreeMap<usize, Anchor>,
    pub unresolved: usize,
    pub total: usize,
}
impl Plan {
    pub(super) fn owner(&self, index: usize) -> Option<Anchor> {
        self.matches.get(&index).copied().or_else(|| {
            self.matches
                .iter()
                .min_by_key(|(i, a)| (i.abs_diff(index), a.rank, a.index))
                .map(|(_, a)| *a)
        })
    }
    pub(super) fn statistics(&self) -> Value {
        json!({"schema":"les.word-read-window.v1","mode":"section-v1",
            "original_units":self.total,"retained_units":self.indices.len(),
            "matched_units":self.matches.len(),"unresolved_anchors":self.unresolved,
            "omitted_units":self.total.saturating_sub(self.indices.len())})
    }
}

fn scope(unit: &Value) -> Vec<String> {
    let path = unit["source_heading_path"].as_array().map(|p| {
        p.iter()
            .filter_map(|h| h["id"].as_str().map(|s| format!("id:{s}")))
            .collect::<Vec<_>>()
    });
    if let Some(path) = path.filter(|p| !p.is_empty()) {
        return path;
    }
    unit["heading"]
        .as_str()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("heading:{s}"))
        .collect()
}
fn inside(parent: &[String], child: &[String]) -> bool {
    if parent.is_empty() {
        child.is_empty()
    } else {
        child.starts_with(parent)
    }
}
fn literal(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn plan(view: &Value, anchors: &[Value], doc: &Value, budget: usize) -> Result<Plan> {
    let units = view["units"]
        .as_array()
        .ok_or("DOCUMENT_STRUCTURE_INVALID")?;
    let mut ordinals = BTreeMap::new();
    let mut hashes = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        let ordinal = u["ordinal"].as_u64().ok_or("DOCUMENT_STRUCTURE_INVALID")?;
        if ordinals.insert(ordinal, i).is_some() {
            return Err("DOCUMENT_STRUCTURE_INVALID".into());
        }
        hashes.push(les_core::sha256_hex(
            u["text"]
                .as_str()
                .ok_or("DOCUMENT_STRUCTURE_INVALID")?
                .as_bytes(),
        ));
    }
    let mut matches = BTreeMap::<usize, Anchor>::new();
    let mut unresolved = 0;
    for (index, point) in anchors.iter().enumerate() {
        let p = &point["payload"];
        if p["dataset_id"] != doc["dataset_id"]
            || p["file_name"] != doc["file_name"]
            || p["source_sha256"] != doc["file_hash"]
            || p["retrieval_expansion"] == true
        {
            continue;
        }
        let exact = p["source_unit_id"]
            .as_u64()
            .and_then(|id| ordinals.get(&id).copied())
            .filter(|&i| {
                p["word_reading_schema"] == "les.document-reading.v1"
                    && p["word_reading_sha256"].as_str() == Some(hashes[i].as_str())
            });
        // A legacy anchor is aligned only by unique literal equality, never by
        // query keywords, ordinal guesses or the first matching heading.
        let located = if exact.is_some() {
            exact
        } else {
            let needle = literal(p["text"].as_str().unwrap_or(""));
            let mut found = units
                .iter()
                .enumerate()
                .filter(|(_, u)| {
                    !needle.is_empty()
                        && (literal(u["text"].as_str().unwrap_or("")) == needle
                            || literal(u["body"].as_str().unwrap_or("")) == needle)
                })
                .map(|(i, _)| i);
            let first = found.next();
            if found.next().is_none() {
                first
            } else {
                None
            }
        };
        if let Some(i) = located {
            let anchor = Anchor {
                index,
                rank: p["retrieval_rank"].as_u64().unwrap_or(index as u64),
            };
            matches
                .entry(i)
                .and_modify(|a| {
                    if (anchor.rank, anchor.index) < (a.rank, a.index) {
                        *a = anchor;
                    }
                })
                .or_insert(anchor);
        } else {
            unresolved += 1;
        }
    }
    let mut indices = matches.keys().copied().collect::<BTreeSet<_>>();
    let scopes = units.iter().map(scope).collect::<Vec<_>>();
    // Required anchor units are never clipped by this surrounding-read
    // allowance, including an oversized atomic table row. The final wire
    // admission remains the sole owner of the answer budget.
    let mut remaining = budget.saturating_sub(
        indices
            .iter()
            .map(|&i| units[i]["text"].as_str().unwrap_or("").chars().count())
            .sum(),
    );
    struct Window {
        hit: usize,
        left: usize,
        right: usize,
        lo: usize,
        hi: usize,
        rank: u64,
        blocked: [bool; 2],
    }
    let mut windows = matches
        .iter()
        .map(|(&i, a)| {
            let mut lo = i;
            let mut hi = i;
            while lo > 0 && inside(&scopes[i], &scopes[lo - 1]) {
                lo -= 1;
            }
            // A directly preceding introduction of the parent can govern
            // this subsection. Read that atom, not unrelated sibling sections.
            if lo > 0
                && !scopes[lo - 1].is_empty()
                && scopes[lo - 1].len() < scopes[i].len()
                && scopes[i].starts_with(&scopes[lo - 1])
                && units[lo - 1]["kind"] != "heading"
            {
                lo -= 1;
            }
            while hi + 1 < units.len() && inside(&scopes[i], &scopes[hi + 1]) {
                hi += 1;
            }
            Window {
                hit: i,
                left: i,
                right: i,
                lo,
                hi,
                rank: a.rank,
                blocked: [false; 2],
            }
        })
        .collect::<Vec<_>>();
    loop {
        let next = windows
            .iter()
            .enumerate()
            .flat_map(|(w, s)| {
                [
                    (0, s.left.checked_sub(1).filter(|&i| i >= s.lo)),
                    (
                        1,
                        (s.right + 1 < units.len())
                            .then_some(s.right + 1)
                            .filter(|&i| i <= s.hi),
                    ),
                ]
                .into_iter()
                .filter(move |(side, _)| !s.blocked[*side])
                .filter_map(move |(side, i)| {
                    i.map(|i| ((i.abs_diff(s.hit), s.rank, side, i), w, side, i))
                })
            })
            .min_by_key(|(priority, _, _, _)| *priority);
        let Some((_, w, side, i)) = next else {
            break;
        };
        let cost = if indices.contains(&i) {
            0
        } else {
            units[i]["text"].as_str().unwrap_or("").chars().count()
        };
        if cost > remaining {
            windows[w].blocked[side] = true;
            continue;
        }
        remaining -= cost;
        indices.insert(i);
        if side == 0 {
            windows[w].left = i;
        } else {
            windows[w].right = i;
        }
    }
    Ok(Plan {
        indices,
        matches,
        unresolved,
        total: units.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc() -> Value {
        json!({"dataset_id":"ds","file_name":"book.docx","file_hash":"a".repeat(64)})
    }
    fn unit(n: usize, section: &str, text: &str) -> Value {
        json!({"ordinal":n,"text":text,"body":text,
        "heading":section,"source_heading_path":[{"id":section}],"source_block_id":format!("b{n}")})
    }
    fn anchor(u: &Value, rank: usize) -> Value {
        json!({"id":format!("anchor-{rank}"),"payload":{
        "dataset_id":"ds","file_name":"book.docx","source_sha256":"a".repeat(64),
        "word_reading_schema":"les.document-reading.v1","source_unit_id":u["ordinal"],
        "word_reading_sha256":les_core::sha256_hex(u["text"].as_str().unwrap().as_bytes()),"retrieval_rank":rank}})
    }
    #[test]
    fn a_late_section_retains_its_table_and_cursor_without_the_rest_of_the_book() {
        let mut units = (0..600)
            .map(|i| unit(i, "earlier", &format!("Other {i}")))
            .collect::<Vec<_>>();
        units.extend([
            unit(600, "target", "Caption"),
            unit(601, "target", "|Item|Value|\n|A|17.25|"),
            unit(602, "target", "Notes and exceptions"),
            unit(603, "next", "Another section"),
        ]);
        units[601]["table_id"] = json!("t");
        units[601]["canonical"] = json!({"kind":"table","rows":[["A","17.25"]]});
        let a = anchor(&units[601], 3);
        let view = json!({"units":units});
        let p = plan(&view, &[a], &doc(), 12000).unwrap();
        assert_eq!(p.indices, BTreeSet::from([600, 601, 602]));
        assert_eq!(p.owner(602).unwrap().index, 0);
        assert_eq!(view["units"][601]["ordinal"], 601);
        assert_eq!(view["units"][601]["canonical"]["rows"][0][1], "17.25");
    }
    #[test]
    fn long_sections_expand_contiguously_around_hits_and_never_truncate_table_rows() {
        let units = (0..101)
            .map(|i| unit(i, "same", &format!("{i:03} {}", "x".repeat(96))))
            .collect::<Vec<_>>();
        let a = anchor(&units[50], 0);
        let view = json!({"units":units});
        let p = plan(&view, &[a.clone()], &doc(), 500).unwrap();
        assert_eq!(p.indices, BTreeSet::from([48, 49, 50, 51, 52]));
        let oversized = plan(&view, &[a], &doc(), 1).unwrap();
        assert_eq!(oversized.indices, BTreeSet::from([50]));
    }
    #[test]
    fn multiple_hits_keep_their_own_retrieval_ids_and_original_ordinals() {
        let units = vec![unit(7, "first", "17.25"), unit(70, "second", "117.25")];
        let anchors = vec![anchor(&units[0], 8), anchor(&units[1], 2)];
        let p = plan(&json!({"units":units}), &anchors, &doc(), 1).unwrap();
        assert_eq!(p.indices, BTreeSet::from([0, 1]));
        assert_eq!(p.matches[&0].index, 0);
        assert_eq!(p.matches[&1].index, 1);
        assert_eq!(p.matches[&1].rank, 2);
    }
    #[test]
    fn an_ambiguous_or_missing_legacy_anchor_never_expands_to_a_whole_document() {
        let units = vec![unit(0, "one", "Repeated"), unit(1, "two", "Repeated")];
        let a = json!({"id":"legacy","payload":{"dataset_id":"ds","file_name":"book.docx","source_sha256":"a".repeat(64),"text":"Repeated"}});
        let p = plan(&json!({"units":units}), &[a], &doc(), 12000).unwrap();
        assert!(p.indices.is_empty());
        assert_eq!(p.unresolved, 1);
    }
    #[test]
    fn nested_sections_are_related_but_a_sibling_section_is_not() {
        let mut units = vec![
            unit(0, "root", "Found"),
            unit(1, "child", "Child"),
            unit(2, "other", "Unrelated"),
        ];
        units[1]["source_heading_path"] = json!([{"id":"root"},{"id":"child"}]);
        let a = anchor(&units[0], 0);
        let p = plan(&json!({"units":units}), &[a], &doc(), 12000).unwrap();
        assert_eq!(p.indices, BTreeSet::from([0, 1]));
    }
    #[test]
    fn a_parent_introduction_survives_without_reading_other_subsections() {
        let mut units = vec![
            unit(0, "root", "Conditions governing the following subsection"),
            unit(1, "child", "Found requirement"),
            unit(2, "other", "Unrelated sibling"),
        ];
        units[1]["source_heading_path"] = json!([{"id":"root"},{"id":"child"}]);
        units[2]["source_heading_path"] = json!([{"id":"root"},{"id":"other"}]);
        let a = anchor(&units[1], 0);
        let p = plan(&json!({"units":units}), &[a], &doc(), 12000).unwrap();
        assert_eq!(p.indices, BTreeSet::from([0, 1]));
    }
}
