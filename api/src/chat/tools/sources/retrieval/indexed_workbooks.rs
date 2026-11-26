//! Resolve explicitly named workbooks and combine their rows with corpus search.
use super::*;

fn words(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() > 1)
        .map(|word| word.to_lowercase().replace('ё', "е"))
}

pub(super) async fn named(context: Arc<Context>, turn: &SourceContext) -> Result<Option<Value>> {
    let question = turn.request["question"].as_str().unwrap_or("");
    let datasets = turn.scope["resolved_dataset_ids"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let names: std::collections::BTreeSet<_> = question
        .split(|c: char| !(c.is_alphanumeric() || "._-".contains(c)))
        .map(|word| word.trim_matches('.'))
        .filter(|word| super::original_reader(word) == "read_excel_source")
        .map(str::to_lowercase)
        .take(4)
        .collect();
    if names.is_empty() || datasets.is_empty() {
        return Ok(None);
    }
    let terms: std::collections::HashSet<_> = words(question).collect();
    let selected = selected_scoped(turn);
    let mut batches = Vec::new();
    for name in names {
        let catalog = super::super::catalog::search(
            context.clone(),
            turn,
            &json!({"q":name,"limit":24}),
            datasets,
            None,
        )
        .await?;
        let mut documents: Vec<_> = catalog["result"]["documents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|doc| {
                let (path, file) = doc["file_name"]
                    .as_str()?
                    .rsplit_once('/')
                    .unwrap_or(("", doc["file_name"].as_str()?));
                if file.to_lowercase() != name
                    || doc["status"] != "INDEXED"
                    || !doc["id"].is_string()
                    || (!selected.is_empty() && !selected.contains(&doc["file_name"]))
                {
                    return None;
                }
                let score = words(path)
                    .filter(|term| terms.contains(term))
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                Some((score, doc))
            })
            .collect();
        documents.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1["file_name"].as_str().cmp(&b.1["file_name"].as_str()))
        });
        let best = documents.first().map_or(0, |v| v.0);
        for (_, doc) in documents
            .into_iter()
            .filter(|item| item.0 == best)
            .take(4usize.saturating_sub(batches.len()))
        {
            let mut args = json!({"doc_id":doc["id"],"q":question,"limit":6,"max_chars":4000});
            super::super::workbook::addressed_range(question, &mut args);
            let Some(result) =
                super::super::workbook::read_indexed(context.clone(), turn, &args).await?
            else {
                continue;
            };
            let mut rows = result["result"]["passages"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if args["row_start"].is_null() {
                let candidate = rows
                    .iter()
                    .filter_map(|p| p["source"]["sheet_name"].as_str())
                    .find(|sheet| words(sheet).any(|word| terms.contains(&word)))
                    .or_else(|| {
                        rows.first()
                            .and_then(|p| p["source"]["sheet_name"].as_str())
                    });
                if let Some(sheet) = candidate {
                    let first = json!({"doc_id":doc["id"],"sheet_name":sheet,"row_start":1,"row_end":10,"limit":10,"max_chars":4500});
                    if let Some(preview) =
                        super::super::workbook::read_indexed(context.clone(), turn, &first).await?
                    {
                        if let Some(mut head) = preview["result"]["passages"].as_array().cloned() {
                            head.retain(|h| {
                                !rows.iter().any(|p| {
                                    h["source"]["sheet_name"] == p["source"]["sheet_name"]
                                        && h["source"]["row"] == p["source"]["row"]
                                })
                            });
                            rows.append(&mut head);
                        }
                    }
                }
            }
            if !rows.is_empty() {
                batches.push(rows);
            }
        }
    }
    if batches.is_empty() {
        return Ok(None);
    }
    // Keep the physical rows of each book together, with room for every book.
    let per_book = 20 / batches.len();
    let passages: Vec<_> = batches
        .into_iter()
        .flat_map(|rows| rows.into_iter().take(per_book))
        .collect();
    let sources = passages.iter().map(|row| row["source"].clone()).collect();
    Ok(Some(envelope(
        "search_sources",
        "indexed_workbook_rows",
        json!({"passages":passages,"coverage":"bounded","selection_policy":{"named_workbooks":true}}),
        sources,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_workbook_keeps_ranked_hits_from_other_files_in_the_window() {
        let row = |number| json!({"source":{"doc_id":"book","sheet_name":"Sheet","row":number}});
        let draw = json!({"source":{"doc_id":"plan","model_locator":{"native_id":"A1"}}});
        let note = json!({"source":{"doc_id":"notes"}});
        let mut ranked = json!({"result":{"selection_policy":{"seed_limit":64},
            "passages":[row(7),draw,note,row(9)]}});
        let named = json!({"result":{"passages":[row(7),row(8)]}});

        merge_named(&mut ranked, named);

        assert_eq!(
            ranked["result"]["passages"],
            json!([row(7), draw, row(8), note, row(9)])
        );
        assert_eq!(ranked["sources"][1], draw["source"]);
        assert_eq!(ranked["result"]["included_fragments"], 5);
        assert_eq!(ranked["result"]["selection_policy"]["seed_limit"], 64);
        assert_eq!(
            ranked["result"]["selection_policy"]["named_workbooks"],
            true
        );
    }
}

pub(super) fn merge_named(result: &mut Value, mut named: Value) {
    let Some(passages) = result["result"]["passages"].as_array_mut() else {
        return;
    };
    let Some(named) = named["result"]["passages"].as_array_mut() else {
        return;
    };
    let named_docs: std::collections::HashSet<_> = named
        .iter()
        .filter_map(|passage| passage["source"]["doc_id"].as_str())
        .collect();
    let (other, same_book): (Vec<_>, Vec<_>) =
        std::mem::take(passages).into_iter().partition(|passage| {
            passage["source"]["doc_id"]
                .as_str()
                .is_none_or(|id| !named_docs.contains(id))
        });
    let same_book: Vec<_> = same_book
        .into_iter()
        .filter(|passage| {
            let source = &passage["source"];
            !named.iter().any(|row| same_row(&row["source"], source))
        })
        .collect();
    // One named row and one hit from the rest of the dataset fit near the head
    // of the model window even when a long book would fill the whole budget.
    let count = named.len().max(other.len());
    let mut rows = std::mem::take(named).into_iter();
    let mut other = other.into_iter();
    for _ in 0..count {
        passages.extend(rows.next());
        passages.extend(other.next());
    }
    passages.extend(same_book);
    update_count(result);
    result["result"]["selection_policy"]["named_workbooks"] = json!(true);
}

fn same_row(left: &Value, right: &Value) -> bool {
    left["sheet_name"].is_string()
        && left["row"].is_u64()
        && ["doc_id", "source_sha256", "sheet_name", "row"]
            .iter()
            .all(|key| left[*key] == right[*key])
}

fn update_count(result: &mut Value) {
    let passages = result["result"]["passages"].as_array().unwrap();
    let count = passages.len();
    result["sources"] = json!(passages
        .iter()
        .map(|passage| &passage["source"])
        .collect::<Vec<_>>());
    result["result"]["included_fragments"] = json!(count);
}

pub(super) async fn append_related(
    context: Arc<Context>,
    turn: &SourceContext,
    result: &mut Value,
) {
    let Some(passages) = result["result"]["passages"].as_array_mut() else {
        return;
    };
    let mut seen = std::collections::HashSet::new();
    let books: Vec<_> = passages
        .iter()
        .take(6)
        .map(|p| &p["source"])
        .filter(|s| {
            s["sheet_name"].is_string()
                && s["doc_id"].is_string()
                && seen.insert(s["doc_id"].clone())
        })
        .take(2)
        .cloned()
        .collect();
    let mut extra = Vec::new();
    for book in books {
        let args = json!({"doc_id":book["doc_id"],"source_sha256":book["source_sha256"],
            "q":turn.request["question"],"limit":5,"max_chars":3000});
        if let Ok(Some(rows)) =
            super::super::workbook::read_indexed(context.clone(), turn, &args).await
        {
            for passage in rows["result"]["passages"].as_array().into_iter().flatten() {
                let source = &passage["source"];
                if !passages
                    .iter()
                    .chain(&extra)
                    .any(|candidate| same_row(&candidate["source"], source))
                {
                    extra.push(passage.clone());
                }
            }
        }
    }
    if !extra.is_empty() {
        let position = passages.len().min(6);
        passages.splice(position..position, extra);
        update_count(result);
    }
}
