//! Short, source-bound reading units for automatic PDF retrieval.
use super::{Result, Value};
use serde_json::json;

fn separator(line: &str) -> bool {
    line.starts_with('|')
        && line.ends_with('|')
        && line.contains('-')
        && line.chars().all(|c| "|-: \t".contains(c))
}

fn excerpts(
    text: &str,
    query: &str,
    file_name: &str,
    distinct: bool,
) -> Result<Vec<(usize, String)>> {
    let file = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let title = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let prepared = les_core::lexical::reading(&[query.to_owned(), title.to_owned()])?;
    let terms = prepared["documents"][0]["terms"]
        .as_object()
        .ok_or("INDEX_QUERY_INVALID")?;
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let lines: Vec<_> = text
        .lines()
        .flat_map(|line| line.split('\r'))
        .map(str::trim)
        .collect();
    let mut candidates = Vec::new();
    let mut header = None;
    for (i, &line) in lines.iter().enumerate() {
        if line.starts_with('|') && line.ends_with('|') {
            if lines.get(i + 1).is_some_and(|next| separator(next)) {
                header = Some((line, lines[i + 1]));
                continue;
            }
        } else {
            header = None;
        }
        if line.is_empty() || line.starts_with('#') || separator(line) {
            continue;
        }
        // A physical row or line stays intact. Keep wrapped words and a number
        // on the following line with their label when the text layer splits them.
        let next = lines.get(i + 1).copied().unwrap_or("");
        let continuation = !next.is_empty()
            && (line.ends_with('-') && next.starts_with(char::is_lowercase)
                || next.chars().any(|c| c.is_ascii_digit())
                    && next
                        .chars()
                        .all(|c| c.is_ascii_digit() || " .,+−-".contains(c)));
        let body = if continuation && header.is_none() {
            format!("{line}\n{next}")
        } else {
            line.to_owned()
        };
        candidates.push((i, body, header));
    }
    let mut keys: Vec<_> = terms
        .keys()
        .filter(|key| prepared["documents"][1]["terms"].get(*key).is_none())
        .collect();
    if keys.is_empty() {
        keys.extend(terms.keys());
    }
    let surface = les_core::unicode::lexical(query);
    let words: Vec<_> = surface
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 5)
        .collect();
    // A short stem can lose a match that keeps the whole word after inflection.
    let alternatives: Vec<Vec<_>> = keys
        .iter()
        .map(|key| {
            words
                .iter()
                .copied()
                .filter(|word| word.len() > key.len() && word.starts_with(key.as_str()))
                .collect()
        })
        .collect();
    let mut frequencies = vec![0usize; keys.len()];
    let mut matches = Vec::new();
    for (batch, rows) in candidates.chunks(256).enumerate() {
        let texts: Vec<_> = rows.iter().map(|(_, text, _)| text.clone()).collect();
        let analysed = les_core::lexical::reading(&texts)?;
        for (i, document) in analysed["documents"].as_array().unwrap().iter().enumerate() {
            let matched: Vec<_> = keys
                .iter()
                .enumerate()
                .filter_map(|(t, &key)| {
                    let terms = &document["terms"];
                    (terms.get(key).is_some()
                        || alternatives[t]
                            .iter()
                            .any(|word| terms.get(*word).is_some()))
                    .then_some(t)
                })
                .collect();
            if !matched.is_empty() {
                for &term in &matched {
                    frequencies[term] += 1;
                }
                matches.push((batch * 256 + i, matched));
            }
        }
    }
    let mixed_script = query.chars().any(|c| c.is_alphabetic() && !c.is_ascii());
    let weights: Vec<_> = frequencies
        .into_iter()
        .zip(keys)
        .map(|(n, term)| {
            let code = mixed_script
                && term.len() >= 2
                && term.is_ascii()
                && term.bytes().any(|c| c.is_ascii_alphabetic());
            let idf = (1.0 + (candidates.len() as f64 - n as f64 + 0.5) / (n as f64 + 0.5)).ln();
            if code {
                2.0 * idf
            } else {
                idf
            }
        })
        .collect();
    let mut ranked: Vec<_> = matches
        .into_iter()
        .map(|(i, found)| {
            let body = &candidates[i].1;
            let words = body
                .split_whitespace()
                .filter(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 3)
                .count();
            // Favor actual labels over bare codes/coordinates repeated on a drawing.
            let score = found.iter().map(|&t| weights[t]).sum::<f64>();
            (
                i,
                score
                    * match words {
                        0 => 0.35,
                        1 => 0.75,
                        _ => 1.0,
                    }
                    / (body.chars().count() as f64 / 800.0).max(1.0),
                found,
            )
        })
        .collect();
    ranked.sort_by(|(a, score_a, _), (b, score_b, _)| {
        score_b.total_cmp(score_a).then_with(|| a.cmp(b))
    });
    let mut seen = std::collections::HashSet::new();
    let mut covered = std::collections::HashSet::new();
    let mut excerpts = Vec::new();
    let mut related = Vec::new();
    for (i, _, found) in ranked {
        let (line, body, header) = &candidates[i];
        if !seen.insert((body, header)) {
            continue;
        }
        let adds_terms = found.iter().any(|term| !covered.contains(term));
        covered.extend(found);
        let text = match header {
            Some((label, divider)) => format!("{label}\n{divider}\n{body}"),
            None => body.clone(),
        };
        if distinct && !adds_terms {
            related.push((*line, text));
        } else {
            excerpts.push((*line, text));
        }
    }
    excerpts.extend(related);
    Ok(excerpts)
}

fn canonical(text: &str) -> Value {
    // A table excerpt already carries its real Markdown header and row.
    if text.starts_with('|') {
        Value::Null
    } else {
        json!({"schema":"les.document-structure.v1","kind":"paragraph","text":text})
    }
}

pub(super) fn is_pdf(payload: &Value) -> bool {
    payload["page"].is_u64()
        && payload["file_name"].as_str().is_some_and(|file| {
            file.rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("pdf"))
        })
}

pub(super) fn bare_code(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(code) = words.next() else {
        return false;
    };
    code.len() >= 2
        && code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-')
        && code.bytes().any(|c| c.is_ascii_uppercase())
        && words.all(|word| word.chars().all(|c| !c.is_alphabetic()))
}

pub(super) fn index_excerpts(
    points: Vec<Value>,
    query: &str,
    selected_files: &[Value],
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    // An exceptionally dense page can stay whole. The bound here is the
    // context selector's input limit, not a relevance cutoff on each hit.
    let materialization_limit = les_core::context_selection::MAX_SOURCE_UNITS / points.len().max(1);
    for mut point in points {
        let payload = &point["payload"];
        if !is_pdf(payload) {
            result.push(point);
            continue;
        }
        let text = if let Some(view) = payload["pdf_page_view"]
            .as_str()
            .filter(|_| payload["physical_pdf_read"].is_null())
        {
            if payload["pdf_page_view_schema"] != "les.pdf-page-view.v1"
                || view.chars().count() > 16000
                || payload["pdf_page_view_sha256"] != les_core::sha256_hex(view.as_bytes())
            {
                return Err("PDF_PAGE_VIEW_INVALID".into());
            }
            view
        } else {
            payload["text"].as_str().unwrap_or("")
        };
        let file = payload["file_name"].as_str();
        let reading_question = crate::chat::inspection::read_question(query, file, selected_files);
        let matches = excerpts(text, reading_question, file.unwrap_or(""), false)?;
        if matches.is_empty() || matches.len() > materialization_limit {
            result.push(point);
            continue;
        }
        let anchor = payload
            .get("retrieval_anchor_id")
            .unwrap_or(&point["id"])
            .clone();
        // A page view can have a different carrier than its ranked anchor.
        // Keep the underlying point id for the index receipt and give each
        // excerpt a separate context identity.
        let id = point["id"].to_string();
        let body = point["payload"]
            .as_object_mut()
            .ok_or("INDEX_READ_INVALID")?;
        body.remove("pdf_page_view");
        body.remove("text");
        if let Some(read) = body
            .get_mut("physical_pdf_read")
            .and_then(Value::as_object_mut)
        {
            read.remove("text");
            read.remove("next_offset");
            read.insert("selection".into(), json!("structured_page_unit"));
            read.insert("complete".into(), json!(false));
        }
        for (line, text) in matches {
            let mut excerpt = point.clone();
            excerpt["id"] = json!(format!("pdf-excerpt:{id}:{line}"));
            let body = &mut excerpt["payload"];
            body["canonical"] = canonical(&text);
            body["reading_unit_sha256"] = json!(les_core::sha256_hex(text.as_bytes()));
            body["text"] = json!(text);
            body["retrieval_anchor_id"] = anchor.clone();
            body["pdf_excerpt_line"] = json!(line);
            body["reading_representation"] = json!("pdf_excerpt");
            result.push(excerpt);
        }
    }
    Ok(result)
}

pub(in crate::chat) fn focus_page(reply: &mut Value, query: &str, max_chars: usize) -> Result<()> {
    let Some(text) = reply["result"]["text"].as_str() else {
        return Ok(());
    };
    let source = &reply["sources"][0];
    let matches = excerpts(text, query, source["doc_name"].as_str().unwrap_or(""), true)?;
    if matches.is_empty() {
        return Ok(());
    }
    let passages: Vec<_> = matches
        .into_iter()
        .map(|(line, text)| {
            let mut source = source.clone();
            source["evidence_kind"] = json!("physical_pdf_unit");
            source["pdf_excerpt_line"] = json!(line);
            source["reading_unit_sha256"] = json!(les_core::sha256_hex(text.as_bytes()));
            source["snippet"] = json!(text.chars().take(240).collect::<String>());
            json!({"canonical":canonical(&text),"text":text,"source":source})
        })
        .collect();
    reply["sources"] = json!(passages.iter().map(|p| &p["source"]).collect::<Vec<_>>());
    let result = reply["result"]
        .as_object_mut()
        .ok_or("PDF_SOURCE_INVALID")?;
    result.remove("text");
    result.insert("passages".into(), json!(passages));
    result.insert("selection".into(), json!("query_excerpts"));
    result.insert("complete".into(), json!(false));
    super::retrieval::bound_pdf_excerpts(reply, max_chars)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_focus_does_not_discard_other_ranked_pages() {
        let point = |page, text| {
            json!({"id":format!("p{page}"),"payload":{
                "page":page,"file_name":"Report.pdf","dataset_id":"scope","text":text
            }})
        };
        let result = index_excerpts(
            vec![
                point(1, "Pump flow 12 litres"),
                point(2, "Discharge 36 cubic metres"),
                point(3, "Head 9 metres"),
            ],
            "pump flow",
            &[],
        )
        .unwrap();
        assert_eq!(
            result
                .iter()
                .map(|p| &p["payload"]["page"])
                .collect::<Vec<_>>(),
            [&json!(1), &json!(2), &json!(3)]
        );
        assert_eq!(result[0]["payload"]["retrieval_anchor_id"], "p1");
        assert_eq!(result[1]["id"], "p2");
    }

    #[test]
    fn a_single_hit_can_offer_five_rows_to_the_budget_even_with_unequal_scores() {
        let text = "Pump flow 12 litres\nPump pressure 9 bar\nPump capacity 7 kilowatts\nPump mass 42 kg\nValve diameter 500 mm";
        let rows = excerpts(
            text,
            "pump flow pressure capacity mass valve diameter",
            "Report.pdf",
            true,
        )
        .unwrap();
        assert_eq!(rows.len(), 5);
        let mut reply = json!({"result":{"text":text},"sources":[{"doc_name":"Report.pdf","doc_id":"doc","page":1}]});
        focus_page(
            &mut reply,
            "pump flow pressure capacity mass valve diameter",
            3000,
        )
        .unwrap();
        assert_eq!(reply["result"]["passages"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn reading_a_repeated_drawing_code_prefers_its_label() {
        let text = "## Page 1\nCP 14\nCP 22\nCP Точка консолидации\nПроверил Иванов";
        let found = excerpts(
            text,
            "Что означает CP на структурной схеме?",
            "Структурная схема.pdf",
            true,
        )
        .unwrap();
        assert_eq!(found[0].1, "CP Точка консолидации");
    }

    #[test]
    fn pdf_table_excerpt_keeps_the_source_header_and_the_complete_matching_row() {
        let text = "| Позиция | Наименование | Количество |\n| --- | --- | --- |\n| 3.1 | Коробка для полых стен | 830 |\n| 3.2 | Прожектор | 17 |";
        let found = excerpts(
            text,
            "Сколько коробок для полых стен?",
            "Ведомость.pdf",
            true,
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, text.rsplit_once('\n').unwrap().0);
    }

    #[test]
    fn inflected_word_keeps_the_needed_line_ahead_of_near_duplicates() {
        let text =
            "Терминирование кабеля двухпортовой розеткой\rТерминирование кабеля разъемом RJ-45";
        let found = excerpts(
            text,
            "Какой разъём для терминирования кабеля?",
            "Схема.pdf",
            true,
        )
        .unwrap();
        assert!(found[0].1.ends_with("RJ-45"));
    }
}
