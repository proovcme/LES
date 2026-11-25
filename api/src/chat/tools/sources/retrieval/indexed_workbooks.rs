//! Resolve explicitly named workbooks in the selected folder before ranking their rows.
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
                            rows.retain(|p| {
                                !head.iter().any(|h| {
                                    h["source"]["sheet_name"] == p["source"]["sheet_name"]
                                        && h["source"]["row"] == p["source"]["row"]
                                })
                            });
                            head.append(&mut rows);
                            rows = head;
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
        json!({"passages":passages,"coverage":"bounded","selection_policy":"named_workbooks"}),
        sources,
    )))
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
                if !passages.iter().chain(&extra).any(|candidate| {
                    ["doc_id", "sheet_name", "row"]
                        .iter()
                        .all(|key| candidate["source"][*key] == source[*key])
                }) {
                    extra.push(passage.clone());
                }
            }
        }
    }
    if !extra.is_empty() {
        let position = passages.len().min(6);
        passages.splice(position..position, extra);
        let count = passages.len();
        let sources = json!(passages
            .iter()
            .map(|passage| &passage["source"])
            .collect::<Vec<_>>());
        result["sources"] = sources;
        result["result"]["included_fragments"] = json!(count);
    }
}
