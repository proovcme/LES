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
        let prepared = crate::lexical::versioned(batch, crate::lexical::SNOWBALL)?;
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
        let key = format!(
            "{}\0{}",
            if unit["self_contained"] == true {
                String::new()
            } else {
                normalized(unit["heading"].as_str().unwrap_or(""))
            },
            body_key
        );
        if let Some(&index) = keys.get(&key) {
            let previous: &mut Value = &mut units[index];
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
    let mut weights = BTreeMap::new();
    for term in q {
        let df = body.iter().filter(|t| t.contains(term)).count();
        if df > 0 {
            weights.insert(
                term.clone(),
                (1.0 + (units.len() as f64 - df as f64 + 0.5) / (df as f64 + 0.5)).ln(),
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
                    0.0
                } else {
                    10.0 / (10.0 + rank)
                };
                0.35 * prior + 0.65 * coverage(&body[i])
            }
        })
        .collect();
    let mut chosen: Vec<usize> = Vec::new();
    let mut covered = BTreeSet::new();
    let mut used = 0;
    let mut text_used = 0;
    while chosen.len() < limit {
        let mut best: Option<(usize, f64)> = None;
        for i in 0..units.len() {
            if chosen.contains(&i)
                || relevance[i] == 0.0
                || (group_selection && relevance[i] == 0.0)
                || used + cost(&units[i], identity_reserve) > budget
                || text_used + units[i]["text"].as_str().unwrap().chars().count() > text_budget
            {
                continue;
            }
            let novel = body[i].difference(&covered).cloned().collect();
            let redundancy = chosen
                .iter()
                .map(|&j| {
                    let union = body[i].union(&body[j]).count().max(1);
                    body[i].intersection(&body[j]).count() as f64 / union as f64
                })
                .fold(0.0_f64, f64::max);
            // Weighted coverage + maximal marginal relevance. No topic rules,
            // file-name boosts, hidden model, or answer-dependent thresholds.
            // Budgeted MMR uses marginal value per sqrt(cost): a long parent
            // cannot consume the window merely by containing more query words.
            let score = if total_weight == 0.0 {
                // No lexical signal: keep retrieval order, rather than
                // pretending text dissimilarity established relevance.
                relevance[i]
            } else {
                (0.7 * relevance[i] + 0.3 * coverage(&novel) - 0.3 * redundancy)
                    / (cost(&units[i], identity_reserve).max(1) as f64).sqrt()
            };
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((i, score));
            }
        }
        let Some((index, _)) = best else {
            break;
        };
        used += cost(&units[index], identity_reserve);
        text_used += units[index]["text"].as_str().unwrap().chars().count();
        covered.extend(body[index].iter().cloned());
        chosen.push(index);
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
