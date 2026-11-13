//! Lexical filtering within a registered document respects the core batch limit.
use super::*;
pub(super) fn rank(points: Vec<Value>, query: String) -> Result<Vec<Value>> {
    let query = les_core::lexical::current(&[query])?;
    let terms = query["documents"][0]["terms"]
        .as_object()
        .ok_or("INDEX_QUERY_INVALID")?;
    let mut ranked = Vec::new();
    for batch in points.chunks(256) {
        let texts: Vec<String> = batch
            .iter()
            .map(|p| p["payload"]["text"].as_str().unwrap_or("").into())
            .collect();
        let tokens = les_core::lexical::current(&texts)?;
        for (i, point) in batch.iter().enumerate() {
            let count = terms
                .keys()
                .filter(|key| tokens["documents"][i]["terms"].get(*key).is_some())
                .count();
            if count > 0 {
                ranked.push((count, point.clone()));
            }
        }
    }
    ranked.sort_by_key(|(score, point)| {
        (
            std::cmp::Reverse(*score),
            point["payload"]["chunk_ord"].as_u64().unwrap_or(0),
        )
    });
    Ok(ranked.into_iter().map(|(_, point)| point).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_answer_after_the_first_256_fragments_is_still_readable() {
        let mut points: Vec<_> = (0..600)
            .map(|i| json!({"id":i,"payload":{"text":"Other paragraphs","chunk_ord":i}}))
            .collect();
        points[599]["payload"]["text"] = json!("Controlled marker 17.35 kW");
        let ranked = rank(points, "Controlled marker".into()).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0]["id"], 599);
    }
}
