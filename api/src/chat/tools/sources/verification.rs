//! A final citation must still resolve to the original version actually read.
use super::{document, Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
fn origins(sources: &Value) -> Result<Vec<&Value>> {
    let mut pending = sources.as_array().into_iter().flatten().collect::<Vec<_>>();
    let mut found = Vec::new();
    while let Some(source) = pending.pop() {
        if found.len() >= 16384 {
            return Err("SOURCE_IDENTITY_INVALID".into());
        }
        pending.extend(
            source["equivalent_sources"]
                .as_array()
                .into_iter()
                .flatten(),
        );
        found.push(source);
    }
    Ok(found)
}
pub(crate) async fn physical(context: Arc<Context>, sources: &Value) -> Result<()> {
    originals(context, sources, &[]).await
}
pub(crate) async fn originals(
    context: Arc<Context>,
    sources: &Value,
    required: &[Value],
) -> Result<()> {
    let mut seen = HashMap::new();
    for source in origins(sources)?.into_iter().filter(|s| {
        required.iter().any(|d| d["id"] == s["doc_id"])
            || s["prepared_evidence_origin"] == true
            || matches!(
                s["evidence_kind"].as_str(),
                Some(
                    "physical_pdf_page"
                        | "indexed_pdf_row"
                        | "physical_pdf_unit"
                        | "physical_word_section"
                        | "physical_workbook_row"
                )
            )
    }) {
        let id = source["doc_id"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?;
        let expected = source["source_sha256"]
            .as_str()
            .filter(|v| v.len() == 64)
            .ok_or("SOURCE_IDENTITY_INVALID")?
            .to_owned();
        let identity = (source["dataset_id"].clone(), source["doc_name"].clone());
        if let Some(previous) = seen.insert((id.to_owned(), expected.clone()), identity.clone()) {
            if previous != identity {
                return Err("SOURCE_IDENTITY_INVALID".into());
            }
            continue;
        }
        let doc = document(&context, &json!({"doc_id":id})).await?;
        if doc["dataset_id"] != source["dataset_id"] || doc["file_name"] != source["doc_name"] {
            return Err("SOURCE_REVISION_CHANGED".into());
        }
        let content = context.state_root.join("storage/datasets");
        tokio::task::spawn_blocking(move || -> Result<()> {
            let path = les_core::document_source::resolve(
                doc["source_path"].as_str().unwrap_or(""),
                doc["dataset_id"]
                    .as_str()
                    .ok_or("SOURCE_IDENTITY_INVALID")?,
                doc["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?,
                &content,
            )
            .map_err(|_| "SOURCE_VERIFICATION_UNAVAILABLE")?;
            let mut file =
                std::fs::File::open(path).map_err(|_| "SOURCE_VERIFICATION_UNAVAILABLE")?;
            let actual = les_core::pdf_source::fingerprint(&mut file)
                .map_err(|_| "SOURCE_VERIFICATION_UNAVAILABLE")?;
            if actual != expected {
                return Err("SOURCE_REVISION_CHANGED".into());
            }
            Ok(())
        })
        .await
        .map_err(|_| "SOURCE_VERIFICATION_UNAVAILABLE")??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    #[test]
    fn shared_text_does_not_hide_a_second_original_from_final_verification() {
        let sources = json!([{"doc_id":"first","equivalent_sources":[
            {"doc_id":"second","shared_text_origin":true},
            {"doc_id":"third","evidence_kind":"physical_word_section"}]}]);
        let ids = origins(&sources)
            .unwrap()
            .into_iter()
            .map(|s| s["doc_id"].as_str().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(ids, HashSet::from(["first", "second", "third"]));
    }
}
