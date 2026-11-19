//! Lexical filtering within a registered document respects the core batch limit.
use super::*;
pub(super) fn rank(mut points: Vec<Value>, query: String, automatic: bool) -> Result<Vec<Value>> {
    let query = les_core::lexical::reading(&[query])?;
    let terms = query["documents"][0]["terms"]
        .as_object()
        .ok_or("INDEX_QUERY_INVALID")?;
    let terms: Vec<_> = terms.keys().collect();
    let mut frequencies = vec![0usize; terms.len()];
    let mut hits = Vec::new();
    for (chunk, batch) in points.chunks(256).enumerate() {
        let texts: Vec<String> = batch
            .iter()
            .map(|p| p["payload"]["text"].as_str().unwrap_or("").into())
            .collect();
        let tokens = les_core::lexical::reading(&texts)?;
        for i in 0..batch.len() {
            let mut matched = Vec::new();
            for (term, key) in terms.iter().enumerate() {
                if tokens["documents"][i]["terms"].get(*key).is_some() {
                    frequencies[term] += 1;
                    matched.push(term);
                }
            }
            if !matched.is_empty() {
                hits.push((chunk * 256 + i, matched));
            }
        }
    }
    let ordinal = |p: &Value| p["payload"]["chunk_ord"].as_u64().unwrap_or(0);
    if automatic && hits.is_empty() {
        points.sort_by_key(ordinal);
        return Ok(points);
    }
    let weights: Vec<_> = frequencies
        .into_iter()
        .map(|count| {
            if automatic {
                // Corpus-local IDF keeps repeated boilerplate in a long question
                // from outranking the few terms that identify the requested section.
                let count = count as f64;
                (1.0 + (points.len() as f64 - count + 0.5) / (count + 0.5)).ln()
            } else {
                1.0
            }
        })
        .collect();
    let mut ranked: Vec<_> = hits
        .into_iter()
        .map(|(i, terms)| (i, terms.iter().map(|&t| weights[t]).sum::<f64>()))
        .collect();
    ranked.sort_by(|(a, score_a), (b, score_b)| {
        score_b
            .total_cmp(score_a)
            .then_with(|| ordinal(&points[*a]).cmp(&ordinal(&points[*b])))
    });
    Ok(ranked
        .into_iter()
        .map(|(i, _)| std::mem::take(&mut points[i]))
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compound_product_names_are_readable_by_their_natural_word_forms() {
        let points = vec![
            json!({"id":1,"payload":{"text":"IP-видеокамера купольная; количество 17 шт.","chunk_ord":0}}),
            json!({"id":2,"payload":{"text":"AXIS-P1375; quantity 117.25","chunk_ord":1}}),
        ];
        assert_eq!(
            rank(points.clone(), "видеокамеры".into(), false).unwrap()[0]["id"],
            1
        );
        assert!(rank(points, "17.25".into(), false).unwrap().is_empty());
    }
    #[test]
    fn an_answer_after_the_first_256_fragments_is_still_readable() {
        let mut points: Vec<_> = (0..600)
            .map(|i| json!({"id":i,"payload":{"text":"Other paragraphs","chunk_ord":i}}))
            .collect();
        points[599]["payload"]["text"] = json!("Controlled marker 17.35 kW");
        let ranked = rank(points, "Controlled marker".into(), false).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0]["id"], 599);
    }

    #[test]
    fn automatic_focus_prefers_distinctive_terms_and_can_open_an_unmatched_document() {
        let points = ["Document system", "Optical fiber", "Document system"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| json!({"id":i,"payload":{"text":text,"chunk_ord":i}}))
            .collect::<Vec<_>>();
        assert_eq!(
            rank(points.clone(), "document fiber".into(), true).unwrap()[0]["id"],
            1
        );
        let fallback = rank(points.into_iter().rev().collect(), "pump".into(), true).unwrap();
        assert_eq!(
            fallback
                .iter()
                .map(|r| r["id"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }
}
