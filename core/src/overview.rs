//! Overview discovery uses the storage engines' document groups before top-k.
//! Dataset-local requests avoid conflating equal file names in different scopes.
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, &'static str>;

pub fn dense_requests(query: &Value, datasets: &Value) -> Result<Vec<Value>> {
    let datasets = datasets
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or("OVERVIEW_SCOPE_INVALID")?;
    let mut seen = std::collections::BTreeSet::new();
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

/// Keep the original scores/order for RRF; group enumeration is not a new rank.
/// Call after each dataset reply so retained memory is bounded by top-k.
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
    let mut seen = std::collections::BTreeSet::new();
    retained.retain(|p| seen.insert(p["id"].to_string()));
    retained.truncate(limit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        merge_dense(&mut points,&json!({"groups":[{"hits":[{"id":2,"score":0.3},{"id":3,"score":0.9}]},{"hits":[{"id":1,"score":0.7}]}]}),2).unwrap();
        merge_dense(
            &mut points,
            &json!({"groups":[{"hits":[{"id":4,"score":0.8}]}]}),
            2,
        )
        .unwrap();
        assert_eq!(
            points.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(),
            vec![json!(3), json!(4)]
        );
        assert!(dense_requests(&json!({}), &json!([])).is_err());
    }
}
