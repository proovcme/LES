//! Resolve the user's document requirements before reading or ranking any text.
use super::*;
use crate::chat::error::Failure;
use std::collections::HashSet;

pub(in crate::chat) struct Selection(Vec<Value>);

impl Selection {
    pub(in crate::chat) async fn resolve(context: &Context, turn: &SourceContext) -> Result<Self> {
        let mut selectors = Vec::new();
        for id in turn.request["scope"]["document_ids"]
            .as_array()
            .into_iter()
            .flatten()
        {
            selectors.push(json!({"id":id}));
        }
        for path in selected_scoped(turn) {
            let path = path.as_str().ok_or("CHAT_SCOPE_INVALID")?;
            selectors.push(if path.contains('/') {
                json!({"path":path})
            } else {
                json!({"short_name":path})
            });
        }
        if selectors.is_empty() {
            return Ok(Self(Vec::new()));
        }
        let database = context.database.clone();
        let args = json!({"dataset_ids":turn.scope["resolved_dataset_ids"], "selectors":selectors});
        let matches = tokio::task::spawn_blocking(move || {
            crate::chat::unwrap(crate::library::catalog(
                &database,
                "library_selection",
                args,
            ))
        })
        .await
        .map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
        let mut rows = matches
            .as_array()
            .ok_or("SOURCE_CATALOG_UNAVAILABLE")?
            .iter()
            .peekable();
        let mut documents = Vec::new();
        let mut ids = HashSet::new();
        for (i, selector) in selectors.iter().enumerate() {
            let label = selector["id"]
                .as_str()
                .or(selector["path"].as_str())
                .or(selector["short_name"].as_str())
                .ok_or("CHAT_SCOPE_INVALID")?;
            let missing = || Failure::new("SOURCE_DOCUMENT_NOT_FOUND").for_source(label);
            let row = rows
                .next_if(|r| r["position"].as_u64() == Some(i as u64))
                .ok_or_else(missing)?;
            if rows
                .peek()
                .is_some_and(|r| r["position"].as_u64() == Some(i as u64))
            {
                let choices = std::iter::once(row)
                    .chain(
                        rows.by_ref()
                            .take_while(|r| r["position"].as_u64() == Some(i as u64)),
                    )
                    .take(5)
                    .map(|r| {
                        format!(
                            "{} ({})",
                            r["file_name"].as_str().unwrap_or(""),
                            r["dataset_id"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Failure::new("SOURCE_IDENTITY_AMBIGUOUS")
                    .for_source(&format!("{label} — {choices}")));
            }
            if ids.insert(row["id"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?) {
                documents.push(row.clone());
            }
        }
        Ok(Self(documents))
    }
    pub(in crate::chat) fn documents(&self) -> &[Value] {
        &self.0
    }

    pub(in crate::chat) async fn read(
        &self,
        context: Arc<Context>,
        scope: &SourceContext,
        budget: u64,
        mut receive: impl FnMut(&Value),
    ) -> Result<Vec<Value>> {
        let question = scope.request["question"]
            .as_str()
            .ok_or("CHAT_REQUEST_INVALID")?;
        let names: Vec<_> = self.0.iter().map(|doc| doc["file_name"].clone()).collect();
        let mut reads = Vec::new();
        for doc in &self.0 {
            let name = doc["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?;
            let question = crate::chat::inspection::read_question(question, Some(name), &names);
            let reader = retrieval::original_reader(name);
            let mut args = json!({"doc_id":doc["id"],"dataset_id":doc["dataset_id"],"source_sha256":doc["file_hash"]});
            if doc["file_hash"].as_str().is_none_or(|s| s.len() != 64) {
                return Err(Failure::new("SOURCE_IDENTITY_INVALID").for_source(name));
            }
            args["max_chars"] = json!((budget / self.0.len() as u64).clamp(1, 12000));
            let read = if reader == "read_pdf_source" {
                args["page"] = json!(1);
                let first = run_scoped(context.clone(), scope, reader, args.clone())
                    .await
                    .map_err(|e| e.for_source(name))?;
                if first["result"]["page_count"].as_u64().unwrap_or(1) > 1
                    && context.index_store.is_some()
                {
                    // Let the existing within-document reader rank pages when an index exists.
                    args.as_object_mut().unwrap().remove("page");
                    args["q"] = json!(question);
                    args["automatic_focus"] = json!(true);
                    match run_scoped(context.clone(), scope, "read_source", args).await {
                        Err(e)
                            if matches!(
                                e.code.as_str(),
                                "INDEX_NOT_CONFIGURED"
                                    | "INDEX_STORAGE_UNAVAILABLE"
                                    | "SOURCE_SECTION_NOT_FOUND"
                                    | "SOURCE_OFFSET_INVALID"
                            ) =>
                        {
                            Ok(first)
                        }
                        other => other,
                    }
                } else {
                    Ok(first)
                }
            } else {
                args["q"] = json!(question);
                args["automatic_focus"] = json!(true);
                if reader == "read_excel_source" {
                    super::workbook::addressed_range(question, &mut args);
                }
                run_scoped(context.clone(), scope, reader, args).await
            }
            .map_err(|e| e.for_source(name))?;
            if read["tool"] == "read_source" {
                verification::originals(
                    context.clone(),
                    &read["sources"],
                    std::slice::from_ref(doc),
                )
                .await
                .map_err(|e| e.for_source(name))?;
            }
            let body = &read["result"];
            reads.push(json!({"doc_id":doc["id"],"file_name":name,"source_sha256":doc["file_hash"],
                "page":body["page"],"page_count":body["page_count"],"next_offset":body["next_offset"],
                "reading_scope":body["reading_scope"],"coverage":body["coverage"],"included_fragments":body["included_fragments"]}));
            receive(&read);
        }
        Ok(reads)
    }

    pub(in crate::chat) fn reserve(
        &self,
        sources: &[Value],
        optional: &mut Vec<Value>,
        required: &mut Vec<Value>,
    ) {
        for doc in &self.0 {
            let belongs = |block: &Value| {
                block["source_index"]
                    .as_u64()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| sources.get(i as usize))
                    .is_some_and(|s| {
                        s["doc_id"] == doc["id"] && s["source_sha256"] == doc["file_hash"]
                    })
            };
            if !required.iter().any(belongs) {
                if let Some(at) = optional.iter().position(belongs) {
                    required.push(optional.remove(at));
                }
            }
        }
    }
    pub(in crate::chat) fn check_delivery(&self, sources: &Value) -> Result<()> {
        for doc in &self.0 {
            if !sources.as_array().into_iter().flatten().any(|s| {
                s["doc_id"] == doc["id"]
                    && s["dataset_id"] == doc["dataset_id"]
                    && s["source_sha256"] == doc["file_hash"]
            }) {
                return Err(Failure::new("REQUIRED_DOCUMENT_NOT_DELIVERED")
                    .for_source(doc["file_name"].as_str().unwrap_or("Документ")));
            }
        }
        Ok(())
    }
}
