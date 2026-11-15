//! Overview discovery uses the storage engines' document groups before top-k.
//! Dataset-local requests avoid conflating equal file names in different scopes.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
type Result<T> = std::result::Result<T, &'static str>;

pub fn dense_requests(query: &Value, datasets: &Value) -> Result<Vec<Value>> {
    let datasets = datasets
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or("OVERVIEW_SCOPE_INVALID")?;
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for dataset in datasets {
        let dataset = dataset
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or("OVERVIEW_SCOPE_INVALID")?;
        if !seen.insert(dataset) {
            continue;
        }
        let mut request = query.clone();
        let filter = request["filter"].clone();
        request["filter"] = json!({"must":[
            {"key":"dataset_id","match":{"value":dataset}},
            if filter.is_null() { json!({}) } else { filter }
        ]});
        request["group_by"] = json!("file_name");
        request["group_size"] = json!(2);
        if let Some(object) = request.as_object_mut() {
            object.remove("offset");
        }
        result.push(request);
    }
    Ok(result)
}

fn identity(point: &Value) -> Result<String> {
    let p = &point["payload"];
    for field in ["dataset_id", "file_name", "source_sha256"] {
        if p[field].as_str().is_none_or(str::is_empty) {
            return Err("INDEX_SOURCE_IDENTITY_INVALID");
        }
    }
    Ok(json!([p["dataset_id"], p["file_name"], p["source_sha256"]]).to_string())
}

fn groups(points: &[Value]) -> Result<Vec<(String, Vec<Value>)>> {
    let mut positions = BTreeMap::new();
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for p in points {
        let key = identity(p)?;
        let index = *positions.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[index].1.push(p.clone());
    }
    Ok(groups)
}

/// The limit counts documents, not individual anchors. A second anchor from an
/// early document cannot displace the best anchor of a later document.
/// Call after each dataset reply so retained memory is bounded by 2 * top-k.
pub fn merge_dense(retained: &mut Vec<Value>, reply: &Value, limit: usize) -> Result<()> {
    for group in reply["groups"]
        .as_array()
        .ok_or("OVERVIEW_GROUPS_INVALID")?
    {
        for point in group["hits"].as_array().ok_or("OVERVIEW_GROUPS_INVALID")? {
            if !point["score"].as_f64().is_some_and(f64::is_finite) || point["id"].is_null() {
                return Err("OVERVIEW_GROUPS_INVALID");
            }
            retained.push(point.clone());
        }
    }
    retained.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["score"].as_f64().unwrap())
            .then_with(|| a["id"].to_string().cmp(&b["id"].to_string()))
    });
    let mut seen = BTreeSet::new();
    retained.retain(|p| seen.insert(p["id"].to_string()));
    *retained = groups(retained)?
        .into_iter()
        .take(limit)
        .flat_map(|(_, points)| points.into_iter().take(2))
        .collect();
    Ok(())
}

/// Storage groups and score fusion must have the same unit: one document vote
/// per channel, even when dense and BM25 find different anchors in that file.
/// Anchor membership is selected only after document RRF, within the existing
/// read-scope capacity. It never creates additional document votes.
pub fn fuse_documents(
    channels: &[Vec<Value>],
    limit: usize,
    constant: f64,
    method: &str,
    preserve_heads: bool,
) -> Result<Vec<Value>> {
    if channels.is_empty()
        || channels.len() > 2
        || channels.iter().any(|c| c.len() > 4096)
        || limit == 0
        || limit > 512
    {
        return Err("FUSION_LIMIT_INVALID");
    }
    let mut records = BTreeMap::new();
    for p in channels.iter().flatten() {
        if !(p["id"].is_string() || p["id"].as_u64().is_some()) {
            return Err("FUSION_POINT_INVALID");
        }
        if !p["score"].as_f64().is_some_and(f64::is_finite) {
            return Err("FUSION_SCORE_INVALID");
        }
        if records
            .insert(p["id"].to_string(), &p["payload"])
            .is_some_and(|previous| previous != &p["payload"])
        {
            return Err("INDEX_CHANGED_DURING_SEARCH");
        }
    }
    let mut documents = Vec::new();
    let mut anchors = BTreeMap::<String, Vec<Vec<Value>>>::new();
    for (channel, points) in channels.iter().enumerate() {
        let mut projected = Vec::new();
        for (key, points) in groups(points)?.into_iter().take((limit * 2).max(24)) {
            let score = points[0]["score"].clone();
            projected.push(json!({"id":&key,"payload":{"identity":&key},"score":score}));
            anchors
                .entry(key)
                .or_insert_with(|| vec![Vec::new(); channels.len()])[channel] = points;
        }
        documents.push(projected);
    }
    let count = documents.iter().map(Vec::len).sum();
    let ranked =
        crate::retrieval_fusion::retrieve(&documents, count, constant, method, preserve_heads)?;
    let mut bundles = Vec::new();
    for (rank, doc) in ranked.iter().enumerate() {
        let key = doc["id"].as_str().ok_or("FUSION_RESULT_INVALID")?;
        let units =
            crate::retrieval_fusion::retrieve(&anchors[key], limit.min(2), constant, method, true)?;
        let leader = preserve_heads
            && doc["retrieval_fusion"]["channels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["rank"] == 1);
        bundles.push((rank, doc, units, leader));
    }
    let reserved: usize = bundles
        .iter()
        .filter(|(_, _, _, leader)| *leader)
        .map(|(_, _, units, _)| units.len())
        .sum();
    let covered = reserved <= limit;
    let mut remaining = if covered { limit - reserved } else { limit };
    let mut result = Vec::new();
    for (rank, doc, units, leader) in bundles {
        if covered && leader {
        } else if units.len() <= remaining {
            remaining -= units.len();
        } else {
            continue;
        }
        for mut point in units {
            let unit_provenance = point["retrieval_fusion"].clone();
            point["score"] = doc["score"].clone();
            point["payload"]["retrieval_rank"] = json!(rank);
            point["retrieval_fusion"] = json!({"schema":"les.document-fusion.v1","method":method,
                "unit":"document","fusion_rank":rank+1,"selection_rank":result.len()+1,
                "channels":doc["retrieval_fusion"]["channels"],"anchor":unit_provenance,
                "head_coverage":if !preserve_heads{"not_requested"}else if covered{"complete"}else{"insufficient_slots"}});
            result.push(point);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(id: usize, file: &str, score: f64) -> Value {
        json!({"id":id,"score":score,"payload":{"dataset_id":"dataset","file_name":file,
            "source_sha256":"a".repeat(64),"text":format!("unit {id}")}})
    }
    #[test]
    fn different_dense_and_bm25_anchors_are_two_votes_for_one_document() {
        let dense = vec![point(1, "joint.pdf", 0.9), point(3, "common.pdf", 0.8)];
        let lexical = vec![point(2, "joint.pdf", 10.0), point(3, "common.pdf", 9.0)];
        let rows = fuse_documents(&[dense, lexical], 2, 60.0, "rrf", false).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|p| p["payload"]["file_name"] == "joint.pdf"));
        assert_eq!(
            rows.iter()
                .map(|p| p["id"].as_u64().unwrap())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([1, 2])
        );
        for p in rows {
            assert!((p["score"].as_f64().unwrap() - 2.0 / 60.0).abs() < 1e-12);
            assert_eq!(
                p["retrieval_fusion"]["channels"].as_array().unwrap().len(),
                2
            );
            assert_eq!(p["retrieval_fusion"]["unit"], "document");
            assert_eq!(p["payload"]["retrieval_rank"], 0);
        }
    }
    #[test]
    fn changing_a_point_revision_between_channels_is_still_an_index_conflict() {
        let original = point(1, "same.pdf", 0.9);
        let mut replaced = original.clone();
        replaced["payload"]["source_sha256"] = json!("b".repeat(64));
        assert_eq!(
            fuse_documents(&[vec![original], vec![replaced]], 4, 60.0, "rrf", true).unwrap_err(),
            "INDEX_CHANGED_DURING_SEARCH"
        );
    }
    #[test]
    fn channel_heads_remain_readable_without_extra_votes_for_a_long_document() {
        let mut dense = (0..40)
            .map(|i| point(i, "long.pdf", 1.0 - i as f64 / 100.0))
            .collect::<Vec<_>>();
        dense.push(point(50, "dense-only.pdf", 0.2));
        let lexical = vec![
            point(99, "lexical-only.pdf", 30.0),
            point(1, "long.pdf", 20.0),
        ];
        let rows = fuse_documents(&[dense, lexical], 4, 60.0, "rrf", true).unwrap();
        assert!(rows
            .iter()
            .any(|p| p["payload"]["file_name"] == "lexical-only.pdf"));
        assert!(rows.len() <= 4);
        let long = rows
            .iter()
            .find(|p| p["payload"]["file_name"] == "long.pdf")
            .unwrap();
        assert!((long["score"].as_f64().unwrap() - (1.0 / 60.0 + 1.0 / 61.0)).abs() < 1e-12);
    }
    #[test]
    fn scopes_and_scores_survive_storage_group_enumeration() {
        let queries = dense_requests(
            &json!({"query":[1.0],"filter":{"must_not":[{"is_empty":{"key":"text"}}]},"limit":8}),
            &json!(["a", "b", "a"]),
        )
        .unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0]["filter"]["must"][0]["match"]["value"], "a");
        assert_eq!(queries[1]["filter"]["must"][0]["match"]["value"], "b");
        assert_eq!(
            queries[0]["filter"]["must"][1]["must_not"][0]["is_empty"]["key"],
            "text"
        );
        let mut points = vec![];
        merge_dense(&mut points,&json!({"groups":[{"hits":[point(3,"same.pdf",0.9),point(2,"same.pdf",0.8)]},{"hits":[point(1,"later.pdf",0.7)]}]}),2).unwrap();
        merge_dense(
            &mut points,
            &json!({"groups":[{"hits":[point(4,"new.pdf",0.85)]}]}),
            2,
        )
        .unwrap();
        assert_eq!(
            points.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(),
            vec![json!(3), json!(2), json!(4)]
        );
        assert!(dense_requests(&json!({}), &json!([])).is_err());
    }
}
