//! Shared score fusion for Server and Edge. No query/domain-specific rules.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
type Result<T> = std::result::Result<T, &'static str>;

pub fn ranks(r: &Value) -> Result<Value> {
    let constant = r["rank_constant"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.)
        .ok_or("RRF_CONSTANT_INVALID")?;
    let mut positions = HashMap::new();
    let mut rows = Vec::<(Value, f64)>::new();
    for channel in r["channels"].as_array().ok_or("RRF_REQUEST_INVALID")? {
        for (rank, id) in channel
            .as_array()
            .ok_or("RRF_REQUEST_INVALID")?
            .iter()
            .enumerate()
        {
            if !(id.is_string() || id.as_u64().is_some()) {
                return Err("RRF_REQUEST_INVALID");
            }
            let key = id.to_string();
            let index = *positions.entry(key).or_insert_with(|| {
                rows.push((id.clone(), 0.));
                rows.len() - 1
            });
            rows[index].1 += 1. / (constant + rank as f64);
        }
    }
    rows.sort_by(|a, b| b.1.total_cmp(&a.1)); // Stable ties retain first channel's encounter order.
    let limit = r["limit"].as_i64().ok_or("RRF_REQUEST_INVALID")?;
    let count = if limit < 0 {
        rows.len().saturating_sub(limit.unsigned_abs() as usize)
    } else {
        limit as usize
    };
    Ok(
        json!({"hits":rows.into_iter().take(count).map(|(id,score)| json!([id,score])).collect::<Vec<_>>()}),
    )
}
pub fn merge(
    channels: &[Vec<Value>],
    limit: usize,
    constant: f64,
    method: &str,
) -> Result<Vec<Value>> {
    if channels.len() > 8 || channels.iter().any(|c| c.len() > 4096) || limit > 4096 {
        return Err("FUSION_LIMIT_INVALID");
    }
    if !matches!(method, "rrf" | "dbsf") {
        return Err("FUSION_METHOD_INVALID");
    }
    let mut records = HashMap::<String, Value>::new();
    let mut provenance = HashMap::<String, Vec<Value>>::new();
    let mut lists = Vec::new();
    for (channel, rows) in channels.iter().enumerate() {
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for point in rows {
            let id = &point["id"];
            if !(id.is_string() || id.as_u64().is_some()) {
                return Err("FUSION_POINT_INVALID");
            }
            let score = point["score"]
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or("FUSION_SCORE_INVALID")?;
            let key = id.to_string();
            if records
                .get(&key)
                .is_some_and(|p| p["payload"] != point["payload"])
            {
                return Err("INDEX_CHANGED_DURING_SEARCH");
            }
            // Repeated occurrences from one retriever are one vote.
            if !seen.insert(key.clone()) {
                continue;
            }
            records.entry(key.clone()).or_insert_with(|| point.clone());
            provenance
                .entry(key)
                .or_default()
                .push(json!({"channel":channel,"rank":list.len()+1,"score":score}));
            list.push((id.clone(), score));
        }
        lists.push(list);
    }
    let hits = if method == "rrf" {
        ranks(&json!({"action":"fuse","channels":lists.iter().map(|c|c.iter().map(|p|&p.0).collect::<Vec<_>>()).collect::<Vec<_>>(),
            "rank_constant":constant,"limit":limit}))?["hits"].as_array().ok_or("FUSION_RESULT_INVALID")?.clone()
    } else {
        // Standard DBSF: sample deviation, +/-3 sigma endpoints, no clipping.
        let mut scores = HashMap::<String, (Value, f64, usize)>::new();
        let mut encounter = 0;
        for list in &lists {
            let (mut mean, mut m2) = (0.0, 0.0);
            for (i, (_, score)) in list.iter().enumerate() {
                let delta = score - mean;
                mean += delta / (i + 1) as f64;
                m2 += delta * (score - mean);
            }
            let sigma = if list.len() > 1 {
                (m2.max(0.0) / (list.len() - 1) as f64).sqrt()
            } else {
                0.0
            };
            if !mean.is_finite() || !sigma.is_finite() {
                return Err("FUSION_SCORE_INVALID");
            }
            for (id, score) in list {
                let normalized = if sigma == 0.0 {
                    0.5
                } else {
                    (score - mean) / (6.0 * sigma) + 0.5
                };
                if !normalized.is_finite() {
                    return Err("FUSION_SCORE_INVALID");
                }
                let entry = scores.entry(id.to_string()).or_insert_with(|| {
                    encounter += 1;
                    (id.clone(), 0.0, encounter)
                });
                entry.1 += normalized;
            }
        }
        let mut scores = scores.into_values().collect::<Vec<_>>();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
        scores
            .into_iter()
            .take(limit)
            .map(|(id, score, _)| json!([id, score]))
            .collect()
    };
    hits.into_iter().map(|hit| {
        let key=hit[0].to_string();
        let mut point=records.remove(&key).ok_or("FUSION_RESULT_INVALID")?;
        point["score"]=hit[1].clone();
        point["retrieval_fusion"]=json!({"schema":"les.retrieval-fusion.v1","method":method,"channels":provenance.remove(&key).unwrap_or_default()});
        Ok(point)
    }).collect()
}

pub fn execute(request: &Value) -> Result<Value> {
    let channels = request["channels"]
        .as_array()
        .ok_or("FUSION_REQUEST_INVALID")?
        .iter()
        .map(|c| c.as_array().cloned().ok_or("FUSION_REQUEST_INVALID"))
        .collect::<Result<Vec<_>>>()?;
    let limit = request["limit"].as_u64().ok_or("FUSION_LIMIT_INVALID")? as usize;
    Ok(
        json!({"points":retrieve(&channels,limit,request["rank_constant"].as_f64().unwrap_or(10.0),request["method"].as_str().unwrap_or("rrf"),request["preserve_heads"]==true)?}),
    )
}

/// Candidate coverage is separate from relevance scoring. RRF scores do not
/// change. One leader per nonempty retriever survives a bounded initial pool.
pub fn retrieve(
    channels: &[Vec<Value>],
    limit: usize,
    constant: f64,
    method: &str,
    preserve_heads: bool,
) -> Result<Vec<Value>> {
    if !preserve_heads {
        return merge(channels, limit, constant, method);
    }
    if limit > 4096 {
        return Err("FUSION_LIMIT_INVALID");
    }
    let count = channels.iter().map(Vec::len).sum::<usize>();
    let mut ranked = merge(channels, count, constant, method)?;
    for (i, p) in ranked.iter_mut().enumerate() {
        p["retrieval_fusion"]["fusion_rank"] = json!(i + 1);
    }
    let leader = |p: &Value| {
        p["retrieval_fusion"]["channels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["rank"] == 1)
    };
    let covered = ranked.iter().filter(|p| leader(p)).count() <= limit;
    // If there are fewer slots than leaders, preserve relevance order and
    // explicitly report insufficient capacity instead of favoring one channel.
    if covered {
        ranked.sort_by_key(|p| !leader(p));
    }
    ranked.truncate(limit);
    for (i, p) in ranked.iter_mut().enumerate() {
        p["retrieval_fusion"]["selection_rank"] = json!(i + 1);
        p["retrieval_fusion"]["head_coverage"] = json!(if covered {
            "complete"
        } else {
            "insufficient_slots"
        });
    }
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(id: usize, score: f64) -> Value {
        json!({"id":id,"score":score,"payload":{"text":format!("source {id}")}})
    }
    #[test]
    fn rrf_keeps_existing_formula_and_counts_one_vote_per_channel() {
        let channels = vec![
            vec![p(1, 0.9), p(1, 0.9), p(2, 0.8)],
            vec![p(2, 20.0), p(3, 10.0)],
        ];
        let rows = merge(&channels, 3, 10.0, "rrf").unwrap();
        assert_eq!(
            rows.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(),
            vec![json!(2), json!(1), json!(3)]
        );
        assert!((rows[0]["score"].as_f64().unwrap() - (1.0 / 11.0 + 0.1)).abs() < 1e-12);
        assert_eq!(
            rows[1]["retrieval_fusion"]["channels"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(channels[0][0]["score"], 0.9);
    }
    #[test]
    fn dbsf_is_affine_invariant_and_handles_empty_singleton_and_equal_scores() {
        let channels = vec![
            vec![p(1, 0.8), p(2, 0.7), p(3, 0.6)],
            vec![p(4, 30.0), p(2, 20.0), p(1, 10.0)],
        ];
        let rows = merge(&channels, 8, 10.0, "dbsf").unwrap();
        let mut scaled = channels.clone();
        for p in &mut scaled[1] {
            p["score"] = json!(p["score"].as_f64().unwrap() * 100.0 + 2000.0);
        }
        let other = merge(&scaled, 8, 10.0, "dbsf").unwrap();
        for (a, b) in rows.iter().zip(&other) {
            assert_eq!(a["id"], b["id"]);
            assert!((a["score"].as_f64().unwrap() - b["score"].as_f64().unwrap()).abs() < 1e-12);
        }
        assert!(merge(&[vec![]], 4, 10.0, "dbsf").unwrap().is_empty());
        for list in [vec![p(1, 0.2)], vec![p(1, 0.2), p(2, 0.2)]] {
            assert!(merge(&[list], 8, 10.0, "dbsf")
                .unwrap()
                .iter()
                .all(|p| p["score"] == 0.5));
        }
    }
    #[test]
    fn inconsistent_sources_and_unknown_methods_fail_closed() {
        let a = p(1, 1.0);
        let mut b = a.clone();
        b["payload"]["text"] = json!("changed");
        assert_eq!(
            merge(&[vec![a], vec![b]], 8, 10.0, "rrf").unwrap_err(),
            "INDEX_CHANGED_DURING_SEARCH"
        );
        assert_eq!(
            merge(&[], 8, 10.0, "experimental").unwrap_err(),
            "FUSION_METHOD_INVALID"
        );
    }
    #[test]
    fn complementary_leaders_survive_many_weaker_double_votes() {
        for n in [20, 128, 512] {
            let dense = (0..n)
                .map(|i| p(i, 1.0 - i as f64 / (n + 1) as f64))
                .collect::<Vec<_>>();
            let mut lexical = vec![p(n + 1, 100.0)];
            lexical.extend((0..n).map(|i| p(i, 1.0 / (i + 1) as f64)));
            let channels = vec![dense, lexical];
            let baseline = merge(&channels, n + 2, 60.0, "rrf").unwrap();
            let selected = retrieve(&channels, 4, 60.0, "rrf", true).unwrap();
            assert!(selected[..2].iter().any(|p| p["id"] == 0));
            assert!(selected[..2].iter().any(|p| p["id"] == n + 1));
            for p in &selected {
                assert_eq!(
                    p["score"],
                    baseline.iter().find(|b| b["id"] == p["id"]).unwrap()["score"]
                );
            }
            let tiny = retrieve(&channels, 1, 60.0, "rrf", true).unwrap();
            assert_eq!(tiny[0]["id"], baseline[0]["id"]);
            assert_eq!(
                tiny[0]["retrieval_fusion"]["head_coverage"],
                "insufficient_slots"
            );
        }
    }
}
