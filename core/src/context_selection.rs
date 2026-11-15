//! Query-aware, whole-unit context admission. Retrieval scores and the index are immutable.
//! Exact content duplicates share text, never identity or revision authority.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
mod discovery;
pub use discovery::catalog;
// Source volume, not the number of short Word paragraphs, is the primary
// memory bound. Kept below the original 2048 * 100000-character worst case.
pub const MAX_SOURCE_UNITS: usize = 16384;
pub const MAX_SOURCE_TEXT_BYTES: usize = 8 * 1024 * 1024;

fn normalized(text: &str) -> String {
    // Preserve line/cell boundaries, case, punctuation and every digit. In
    // particular, 17.25 != 117.25, and a changed table row is not a duplicate.
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
fn tokens(texts: &[String]) -> Result<Vec<BTreeSet<String>>, &'static str> {
    let mut result = Vec::new();
    for batch in texts.chunks(256) {
        // Context-only morphology; the persisted lexical analyzer, postings
        // and dense vectors are unchanged. Codes/digits keep exact tokens.
        let prepared = crate::lexical::reading(batch)?;
        for row in prepared["documents"]
            .as_array()
            .ok_or("CONTEXT_SELECTION_INVALID")?
        {
            result.push(
                row["terms"]
                    .as_object()
                    .ok_or("CONTEXT_SELECTION_INVALID")?
                    .keys()
                    .cloned()
                    .collect(),
            );
        }
    }
    Ok(result)
}
fn cost(unit: &Value, identity_reserve: usize) -> usize {
    let header = unit["model_header_chars"]
        .as_u64()
        .filter(|v| *v <= 100000)
        .map(|v| v as usize)
        .unwrap_or_else(|| {
            unit["heading"].as_str().unwrap_or("").chars().count()
                + unit["source"].to_string().chars().count()
                + 80
        });
    unit["text"].as_str().unwrap_or("").chars().count() + header.max(identity_reserve)
}
fn content_key(unit: &Value) -> String {
    format!(
        "{}\0{}",
        if unit["self_contained"] == true {
            String::new()
        } else {
            normalized(unit["heading"].as_str().unwrap_or(""))
        },
        normalized(unit["text"].as_str().unwrap_or(""))
    )
}

/// Input units have text, source, unit_id, optional group (physical document),
/// optional rank (retrieved rank; hydrated units inherit their parent's rank).
/// All supplied origins must already have passed scope/revision checks.
pub fn select(request: &Value) -> Result<Value, &'static str> {
    let query = request["question"]
        .as_str()
        .filter(|s| s.len() <= 80000)
        .ok_or("CONTEXT_SELECTION_INVALID")?;
    let input = request["units"]
        .as_array()
        .filter(|v| v.len() <= MAX_SOURCE_UNITS)
        .ok_or("CONTEXT_SELECTION_INVALID")?;
    let bytes = input
        .iter()
        .try_fold(0usize, |n, u| {
            n.checked_add(u["text"].as_str().map_or(0, str::len))
        })
        .ok_or("CONTEXT_SOURCE_LIMIT")?;
    if bytes > MAX_SOURCE_TEXT_BYTES {
        return Err("CONTEXT_SOURCE_LIMIT");
    }
    let budget = request["max_chars"]
        .as_u64()
        .filter(|n| *n <= 1_000_000)
        .ok_or("CONTEXT_SELECTION_INVALID")? as usize;
    let limit = request["max_items"].as_u64().unwrap_or(32).min(64) as usize;
    let text_budget = request["text_budget"].as_u64().unwrap_or(budget as u64) as usize;
    let identity_reserve = request["identity_reserve_chars"]
        .as_u64()
        .unwrap_or(0)
        .min(16384) as usize;
    let mut units: Vec<Value> = Vec::new();
    let originals_pending = request["originals_pending"] == true;
    let mut keys = HashMap::new();
    let mut identities = BTreeSet::new();
    for unit in input {
        if let Some(groups) = request["preferred_groups"].as_array() {
            if !groups.contains(&unit["group"]) {
                continue;
            }
        }
        let text = unit["text"]
            .as_str()
            .filter(|s| s.chars().count() <= 100000)
            .ok_or("CONTEXT_SELECTION_INVALID")?;
        if !unit["source"].is_object() || !unit["unit_id"].is_string() {
            return Err("CONTEXT_SELECTION_INVALID");
        }
        if !identities.insert(json!([unit["source"], unit["unit_id"]]).to_string()) {
            continue;
        }
        let body_key = normalized(text);
        if body_key.is_empty() {
            continue;
        }
        // An equal search excerpt cannot establish equality of unread parents.
        // After original reading, complete equal units can share their text.
        let key = if originals_pending {
            format!(
                "{}\0{}",
                unit.get("read_identity").unwrap_or(&unit["source"]),
                content_key(unit)
            )
        } else {
            content_key(unit)
        };
        if let Some(&index) = keys.get(&key) {
            let previous: &mut Value = &mut units[index];
            if let Some(rank) = unit["source_rank"].as_u64() {
                if previous["source_rank"]
                    .as_u64()
                    .is_none_or(|old| rank < old)
                {
                    previous["source_rank"] = json!(rank);
                }
            }
            if let Some(rank) = unit["rank"].as_u64() {
                if previous["rank"].as_u64().is_none_or(|old| rank < old) {
                    previous["rank"] = json!(rank);
                }
            }
            previous["equivalent_sources"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "source":unit["source"], "unit_id":unit["unit_id"], "group":unit["group"]
                }));
        } else {
            keys.insert(key, units.len());
            let mut unit = unit.clone();
            unit["equivalent_sources"] = json!([]);
            units.push(unit);
        }
    }
    // The copy-only RRF control shares the exact same identity/content
    // normalization, but has no lexical vote or length-based reranking.
    // Ready-JSON admission at the caller owns its budgets, not this ordering.
    match request["policy"].as_str() {
        Some("rrf-order-v1") => {
            units.sort_by_key(|u| {
                let rank = u["rank"].as_u64();
                (
                    rank.is_none(),
                    rank.or_else(|| u["source_rank"].as_u64())
                        .unwrap_or(u64::MAX),
                )
            });
            return Ok(
                json!({"units":units,"selection":{"schema":"les.context-selection.v1",
                "policy":"rrf-order-v1","input_units":input.len(),"complete_corpus":false,
                "budget_owner":"ready_envelope","semantic_selection":false}}),
            );
        }
        None | Some("coverage-mmr-v1") => {}
        _ => return Err("CONTEXT_SELECTION_INVALID"),
    }
    // Vocabulary statistics use unique content, so copies do not outvote an
    // independent source. Names/paths cannot manufacture textual relevance.
    let mut texts = vec![query.to_owned()];
    texts.extend(units.iter().map(|u| {
        format!(
            "{}\n{}",
            u["heading"].as_str().unwrap_or(""),
            u["text"].as_str().unwrap()
        )
    }));
    let terms = tokens(&texts)?;
    let q = &terms[0];
    let body = &terms[1..];
    // Keep exploratory origins separate without letting copied text inflate IDF.
    let mut unique_content = BTreeSet::new();
    let statistics = (0..units.len())
        .filter(|&i| unique_content.insert(content_key(&units[i])))
        .collect::<Vec<_>>();
    let mut weights = BTreeMap::new();
    for term in q {
        let df = statistics
            .iter()
            .filter(|&&i| body[i].contains(term))
            .count();
        if df > 0 {
            weights.insert(
                term.clone(),
                (1.0 + (statistics.len() as f64 - df as f64 + 0.5) / (df as f64 + 0.5)).ln(),
            );
        }
    }
    let total_weight: f64 = weights.values().sum();
    let coverage = |t: &BTreeSet<String>| -> f64 {
        if total_weight == 0.0 {
            return 0.0;
        }
        weights
            .iter()
            .filter(|(k, _)| t.contains(*k))
            .map(|(_, w)| *w)
            .sum::<f64>()
            / total_weight
    };
    let group_selection = request["preferred_groups"].is_array();
    if request.get("preferred_groups").is_some() && !group_selection {
        return Err("CONTEXT_SELECTION_INVALID");
    }
    if request.get("preferred_units").is_some() {
        return Err("CONTEXT_SELECTION_INVALID");
    }
    let preferred_groups: Vec<_> = request["preferred_groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("CONTEXT_SELECTION_INVALID")
        })
        .collect::<Result<_, _>>()?;
    if preferred_groups.len() > 4
        || preferred_groups
            .iter()
            .any(|g| !units.iter().any(|u| u["group"] == *g))
    {
        return Err("CONTEXT_SELECTION_UNKNOWN_UNIT");
    }
    let relevance: Vec<f64> = units
        .iter()
        .enumerate()
        .map(|(i, u)| {
            let rank = u["rank"].as_u64().unwrap_or(i as u64) as f64;
            if group_selection {
                if preferred_groups.iter().any(|g| u["group"] == *g) {
                    0.35 + 0.65 * coverage(&body[i])
                } else {
                    0.0
                }
            } else {
                let prior = if u.get("rank") == Some(&Value::Null) {
                    // A document-level hit is weaker than an exact unit hit:
                    // it contributes only where this original unit matches the
                    // question. Source membership alone never admits a unit.
                    u["source_rank"]
                        .as_u64()
                        .map_or(0.0, |rank| 10.0 / (10.0 + rank as f64) * coverage(&body[i]))
                } else {
                    10.0 / (10.0 + rank)
                };
                0.35 * prior + 0.65 * coverage(&body[i])
            }
        })
        .collect();
    let mut chosen: Vec<usize> = Vec::new();
    let source_limit = request["max_sources"].as_u64().unwrap_or(64).min(64) as usize;
    let groups = units
        .iter()
        .map(|u| {
            u["group"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| u["source"].to_string())
        })
        .collect::<Vec<_>>();
    let mut selected_sources = BTreeSet::new();
    let mut covered = BTreeSet::new();
    let mut used = 0;
    let mut text_used = 0;
    let costs = units
        .iter()
        .map(|u| cost(u, identity_reserve))
        .collect::<Vec<_>>();
    let lengths = units
        .iter()
        .map(|u| u["text"].as_str().unwrap().chars().count())
        .collect::<Vec<_>>();
    // A selected unit never changes. Update maximum redundancy once per new
    // selection instead of comparing every old pair again on every round.
    let mut redundant = vec![0.0_f64; units.len()];
    while chosen.len() < limit {
        let mut best: Option<(usize, f64)> = None;
        for i in 0..units.len() {
            if chosen.contains(&i)
                || (!selected_sources.contains(&groups[i])
                    && selected_sources.len() >= source_limit)
                || relevance[i] == 0.0
                || (group_selection && relevance[i] == 0.0)
                || used + costs[i] > budget
                || text_used + lengths[i] > text_budget
            {
                continue;
            }
            // Only query terms contribute to coverage. Materializing the
            // entire document vocabulary minus `covered` has identical value.
            let novel_coverage = if total_weight == 0.0 {
                0.0
            } else {
                weights
                    .iter()
                    .filter(|(term, _)| body[i].contains(*term) && !covered.contains(*term))
                    .map(|(_, w)| *w)
                    .sum::<f64>()
                    / total_weight
            };
            // Weighted coverage + maximal marginal relevance. No topic rules,
            // file-name boosts, hidden model, or answer-dependent thresholds.
            // Normalize both resource costs by the actual request windows. Absolute
            // inverse length rewarded fragmentation without bound. Every
            // fitting unit now has a bounded length penalty (1..sqrt(2)),
            // while compact complete evidence still leaves room for other facts.
            let score = if total_weight == 0.0 {
                // No lexical signal: keep retrieval order, rather than
                // pretending text dissimilarity established relevance.
                relevance[i]
            } else {
                (0.7 * relevance[i] + 0.3 * novel_coverage - 0.3 * redundant[i])
                    / (1.0
                        + (costs[i] as f64 / budget.max(1) as f64)
                            .max(lengths[i] as f64 / text_budget.max(1) as f64))
                    .sqrt()
            };
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((i, score));
            }
        }
        let Some((index, _)) = best else {
            break;
        };
        used += costs[index];
        text_used += lengths[index];
        covered.extend(body[index].iter().cloned());
        chosen.push(index);
        selected_sources.insert(groups[index].clone());
        for i in 0..units.len() {
            if originals_pending
                && units[i].get("read_identity").unwrap_or(&units[i]["source"])
                    != units[index]
                        .get("read_identity")
                        .unwrap_or(&units[index]["source"])
            {
                continue;
            }
            if relevance[i] == 0.0
                || chosen.contains(&i)
                || used + costs[i] > budget
                || text_used + lengths[i] > text_budget
            {
                continue;
            }
            let intersection = body[i].intersection(&body[index]).count();
            let union = (body[i].len() + body[index].len() - intersection).max(1);
            redundant[i] = redundant[i].max(intersection as f64 / union as f64);
        }
    }
    Ok(
        json!({"units":chosen.iter().map(|&i|units[i].clone()).collect::<Vec<_>>(),
        "selection":{"schema":"les.context-selection.v1","input_units":input.len(),
            "unique_units":units.len(),"selected_units":chosen.len(),"used_chars":used,
            "max_chars":budget,"semantic_selection":group_selection,"used_text_chars":text_used,"query_term_coverage":coverage(&covered),
            "provenance_chars":chosen.iter().map(|&i|units[i]["equivalent_sources"].to_string().chars().count()).sum::<usize>(),
            "complete_corpus":false}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rrf_control_neither_lexically_reranks_nor_spends_the_callers_budget() {
        let mut parent = unit("parent", "query query short", 0);
        parent["rank"] = Value::Null;
        parent["source_rank"] = json!(0);
        let result=select(&json!({"question":"query", "policy":"rrf-order-v1",
            "units":[unit("lexical","query query",8),parent,unit("dense","A semantically related paraphrase without the query term",0)],
            "max_chars":1,"text_budget":1,"max_items":1})).unwrap();
        assert_eq!(
            result["units"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| u["unit_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["dense", "lexical", "parent"]
        );
        assert_eq!(result["selection"]["budget_owner"], "ready_envelope");
        assert_eq!(result["selection"]["semantic_selection"], false);
    }
    #[test]
    fn rrf_control_shares_duplicate_provenance_without_merging_changed_numbers() {
        let first = unit("copy-a", "Pressure 17.25", 5);
        let copy = unit("copy-b", "Pressure 17.25", 0);
        let changed = unit("different", "Pressure 117.250", 1);
        let request = json!({"question":"Pressure", "policy":"rrf-order-v1",
            "units":[first,copy,changed],"max_chars":5000});
        let result = select(&request).unwrap();
        let units = result["units"].as_array().unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0]["rank"], 0);
        assert_eq!(units[0]["equivalent_sources"][0]["unit_id"], "copy-b");
        assert_eq!(units[1]["text"], "Pressure 117.250");
    }
    #[test]
    fn an_independent_text_quota_is_not_hidden_by_a_large_envelope_ceiling() {
        let page = unit(
            "page",
            &format!(
                "Pump pressure and flow. {}",
                "Detailed technical explanation. ".repeat(100)
            ),
            0,
        );
        let row = unit(
            "row",
            "Pump pressure and flow\n|Item|Pressure|Flow|\n|A|17.25 bar|8 l/s|",
            10,
        );
        let text_budget = page["text"].as_str().unwrap().chars().count();
        let result = select(
            &json!({"question":"Pump pressure and flow","units":[page,row],
            "max_chars":1000000,"text_budget":text_budget}),
        )
        .unwrap();
        assert!(result["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit_id"] == "row"));
        assert!(result["selection"]["used_text_chars"].as_u64().unwrap() <= text_budget as u64);
    }
    #[test]
    fn tiny_partial_references_cannot_evict_the_best_complete_unit_by_size_bonus() {
        let complete = unit(
            "complete",
            &format!(
                "Flow 17.25; pressure 2.5; power 60; temperature 20. {}",
                "Additional source information. ".repeat(210)
            ),
            0,
        );
        let mut units = vec![complete];
        for i in 0..12 {
            units.push(unit(
                &format!("fragment-{i}"),
                &format!(
                    "{} drawing reference {i}",
                    ["Flow", "Pressure", "Power", "Temperature"][i % 4]
                ),
                i + 1,
            ));
        }
        let result = select(
            &json!({"question":"Flow pressure power temperature","units":units,
            "max_chars":8000,"max_items":8,"identity_reserve_chars":1024}),
        )
        .unwrap();
        assert!(result["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit_id"] == "complete"));
        assert!(result["selection"]["used_chars"].as_u64().unwrap() <= 8000);
    }
    #[test]
    fn equal_original_units_keep_the_best_document_prior_without_a_unit_vote() {
        let mut low = unit("copy-a", "Pump pressure 17", 0);
        low["rank"] = Value::Null;
        low["source_rank"] = json!(70);
        let mut high = unit("copy-b", "Pump pressure 17", 0);
        high["rank"] = Value::Null;
        high["source_rank"] = json!(0);
        for units in [json!([low.clone(), high.clone()]), json!([high, low])] {
            let result =
                select(&json!({"question":"Pump pressure","units":units,"max_chars":4000}))
                    .unwrap();
            assert_eq!(result["units"].as_array().unwrap().len(), 1);
            assert_eq!(result["units"][0]["source_rank"], 0);
            assert!(result["units"][0]["rank"].is_null());
            assert_eq!(
                result["units"][0]["equivalent_sources"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
    }
    #[test]
    fn legacy_document_prior_requires_content_and_never_invents_unit_rank() {
        let mut hit = unit("located", "Pump pressure rating", 0);
        hit["rank"] = Value::Null;
        hit["source_rank"] = json!(0);
        let mut unrelated = unit("other", "Architectural finishes", 0);
        unrelated["rank"] = Value::Null;
        unrelated["source_rank"] = json!(0);
        let result =
            select(&json!({"question":"Pump pressure", "units":[hit,unrelated],"max_chars":4000}))
                .unwrap();
        let rows = result["units"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["unit_id"], "located");
        assert!(rows[0]["rank"].is_null());
    }
    #[test]
    fn equal_search_excerpts_do_not_collapse_unread_originals() {
        let a = unit("a", "Pump pressure rating 17.25", 0);
        let b = unit("b", "Pump pressure rating 17.25", 1);
        let r = select(
            &json!({"question":"Pump pressure rating","units":[a.clone(),b.clone()],
            "max_chars":4000,"max_items":2,"max_sources":2,"originals_pending":true}),
        )
        .unwrap();
        assert_eq!(r["units"].as_array().unwrap().len(), 2);
        assert!(r["units"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["equivalent_sources"].as_array().unwrap().is_empty()));
        let final_read =
            select(&json!({"question":"Pump pressure rating","units":[a,b],"max_chars":4000}))
                .unwrap();
        assert_eq!(final_read["units"].as_array().unwrap().len(), 1);
        assert_eq!(
            final_read["units"][0]["equivalent_sources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn source_limit_is_a_budget_constraint_not_a_rank_truncation() {
        let mut input = (0..12)
            .map(|i| {
                unit(
                    &format!("source-{i}"),
                    &format!(
                        "General equipment catalogue {i}. {}",
                        "Ordinary description. ".repeat(40)
                    ),
                    i,
                )
            })
            .collect::<Vec<_>>();
        input.push(unit("late", "Pump pressure 17.25 bar; flow 8 litres", 60));
        let result = select(&json!({"question":"Pump pressure and flow","units":input,
            "max_chars":10000,"max_sources":2}))
        .unwrap();
        let rows = result["units"].as_array().unwrap();
        assert!(rows
            .iter()
            .any(|u| u["unit_id"] == "late" && u["rank"] == 60));
        assert!(
            rows.iter()
                .map(|u| u["source"].to_string())
                .collect::<BTreeSet<_>>()
                .len()
                <= 2
        );
    }
    fn unit(id: &str, text: &str, rank: usize) -> Value {
        json!({"unit_id":id,"text":text,"source":{"doc_id":id,"sha":"revision"},"rank":rank})
    }
    #[test]
    fn expanded_document_paragraphs_do_not_inherit_a_hit_at_every_position() {
        let mut table = unit(
            "table",
            "Electrical supply category | two independent transformers | 17.25 kW",
            0,
        );
        table["rank"] = Value::Null;
        let mut units = vec![table.clone()];
        for i in 0..300 {
            let mut footer = unit(&format!("footer-{i}"), &format!("Footer furniture {i}"), 0);
            footer["rank"] = Value::Null;
            units.push(footer);
        }
        let result = select(
            &json!({"question":"Electrical supply category","units":units,"max_chars":4000}),
        )
        .unwrap();
        assert_eq!(result["units"].as_array().unwrap().len(), 1);
        assert_eq!(result["units"][0]["unit_id"], "table");
        // A real dense hit remains admissible without literal query overlap.
        let result = select(&json!({"question":"unmatched synonym","units":[unit("dense","Verified original paragraph",0)],"max_chars":1000})).unwrap();
        assert_eq!(result["units"][0]["unit_id"], "dense");
    }
    #[test]
    fn many_short_paragraphs_keep_the_last_relevant_unit_with_a_byte_bound() {
        let mut units=(0..2200).map(|i|json!({"text":format!("Paragraph {i}"),"source":{"doc_id":"doc"},"unit_id":i.to_string()})).collect::<Vec<_>>();
        units[2199]["text"] = json!("Needle in the final paragraph");
        let result =
            select(&json!({"question":"Needle","units":units,"max_chars":400,"max_items":1}))
                .unwrap();
        assert_eq!(result["units"][0]["unit_id"], "2199");
        let units = (0..2049)
            .map(|i| json!({"text":"x".repeat(4096),"source":{},"unit_id":i.to_string()}))
            .collect::<Vec<_>>();
        assert_eq!(
            select(&json!({"question":"x","units":units,"max_chars":400})).unwrap_err(),
            "CONTEXT_SOURCE_LIMIT"
        );
    }
    #[test]
    fn repeated_pages_share_text_but_keep_every_origin() {
        let a = unit("a", "|Pressure|17.25|\n|Flow|8|", 0);
        let b = unit("b", "|Pressure|17.25|\n|Flow|8|", 1);
        let changed = unit("c", "|Pressure|117.25|\n|Flow|8|", 2);
        let r = select(&json!({"question":"Pressure","units":[a,b,changed],"max_chars":10000}))
            .unwrap();
        assert_eq!(r["selection"]["unique_units"], 2);
        assert_eq!(
            r["units"][0]["equivalent_sources"][0]["source"]["doc_id"],
            "b"
        );
        assert!(r["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit_id"] == "c"));
    }
    #[test]
    fn lower_ranked_complementary_evidence_survives_repeated_boilerplate() {
        let mut units: Vec<_> = (0..30)
            .map(|i| {
                unit(
                    &format!("cover{i}"),
                    "System manual: approved by director; pump manufacturer catalogue",
                    i,
                )
            })
            .collect();
        units.push(unit(
            "rating",
            "Pump pressure rating 17.25 bar; flow capacity 8 litres",
            30,
        ));
        let r = select(&json!({"question":"Pump pressure rating and flow capacity", "units":units,"max_chars":650})).unwrap();
        assert!(r["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit_id"] == "rating"));
        assert!(r["selection"]["used_chars"].as_u64().unwrap() <= 650);
    }
    #[test]
    fn a_long_parent_cannot_spend_the_whole_budget_before_a_compact_table_row() {
        let page = unit(
            "page",
            &format!(
                "Pump pressure and flow. {}",
                "Detailed technical explanation. ".repeat(100)
            ),
            0,
        );
        let row = unit(
            "row",
            "Pump pressure and flow\n|Item|Pressure|Flow|\n|A|17.25 bar|8 l/s|",
            10,
        );
        let budget = cost(&page, 0);
        let r = select(
            &json!({"question":"Pump pressure and flow", "units":[page,row],"max_chars":budget}),
        )
        .unwrap();
        assert!(r["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit_id"] == "row"));
        assert!(r["selection"]["used_chars"].as_u64().unwrap() <= budget as u64);
    }
    #[test]
    fn atomic_budget_never_turns_a_partial_unit_into_complete_text() {
        let r = select(&json!({"question":"answer","units":[unit("a", &"answer ".repeat(100),0),unit("b","answer 17.25",1)],"max_chars":200})).unwrap();
        assert_eq!(r["units"].as_array().unwrap().len(), 1);
        assert_eq!(r["units"][0]["text"], "answer 17.25");
        assert_eq!(r["selection"]["complete_corpus"], false);
    }
    #[test]
    fn whitespace_normalization_does_not_merge_units_or_table_rows() {
        assert_ne!(normalized("A\tB\nC\tD"), normalized("A\tB\tC\tD"));
        assert_ne!(normalized("Pump kW 17.25"), normalized("Pump KW 17.25"));
        assert_eq!(normalized("A  B\r\n\r\nC"), normalized("A B\nC"));
    }
    #[test]
    fn different_column_contexts_are_not_declared_equivalent() {
        let mut a = unit("a", "|Panel A|17.25|", 0);
        let mut b = unit("b", "|Panel A|17.25|", 1);
        a["heading"] = json!("Installed capacity kW");
        b["heading"] = json!("Calculated capacity kW");
        let r =
            select(&json!({"question":"Panel capacity", "units":[a,b],"max_chars":10000})).unwrap();
        assert_eq!(r["selection"]["unique_units"], 2);
    }
    #[test]
    fn a_continuation_inherits_its_verified_heading_without_a_filename_boost() {
        let mut a = unit("a", "Approved and signed. General conditions.", 0);
        a["source"]["file_name"] = json!("Required pump rating pressure manual");
        let mut b = unit("b", "|A|17.25 bar|", 20);
        b["heading"] = json!("Pump pressure rating");
        let r=select(&json!({"question":"Pump pressure rating", "units":[a,b],"max_items":1,"max_chars":10000})).unwrap();
        assert_eq!(r["units"][0]["unit_id"], "b");
    }
    #[test]
    fn selected_document_windows_cannot_admit_a_different_document() {
        let mut a = unit("a", "Generic requirements", 0);
        a["group"] = json!("a");
        let mut b = unit("b", "Verified 17.25 kW", 30);
        b["group"] = json!("b");
        let r = select(
            &json!({"question":"Power","units":[a,b],"preferred_groups":["b"],"max_chars":10000}),
        )
        .unwrap();
        assert_eq!(r["units"].as_array().unwrap().len(), 1);
        assert_eq!(r["units"][0]["source"]["doc_id"], "b");
        assert_eq!(select(&json!({"question":"Power","units":[a,b],"preferred_groups":["foreign"],"max_chars":10000})).unwrap_err(),"CONTEXT_SELECTION_UNKNOWN_UNIT");
        let empty = select(
            &json!({"question":"Power","units":[a,b],"preferred_groups":[],"max_chars":10000}),
        )
        .unwrap();
        assert!(empty["units"].as_array().unwrap().is_empty());
    }
    #[test]
    fn unrendered_provenance_does_not_consume_the_model_text_budget() {
        let mut a = unit("a", "Proof 17.25 kW", 0);
        a["source"]["private_ledger"] = json!("x".repeat(2000));
        a["model_header_chars"] = json!(20);
        let r = select(&json!({"question":"Power","units":[a],"max_chars":100})).unwrap();
        assert_eq!(r["units"].as_array().unwrap().len(), 1);
        assert!(r["selection"]["used_chars"].as_u64().unwrap() <= 100);
        assert_eq!(
            r["units"][0]["source"]["private_ledger"]
                .as_str()
                .unwrap()
                .len(),
            2000
        );
    }
}
