//! The final citation must still resolve to the physical PDF version actually read.
use super::{document, Context, Result};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc};
pub(crate) async fn physical(context: Arc<Context>, sources: &Value) -> Result<()> {
    let mut seen = HashSet::new();
    for source in sources
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["evidence_kind"] == "physical_pdf_page")
    {
        let id = source["doc_id"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?;
        let expected = source["source_sha256"]
            .as_str()
            .filter(|v| v.len() == 64)
            .ok_or("SOURCE_IDENTITY_INVALID")?
            .to_owned();
        if !seen.insert((id.to_owned(), expected.clone())) {
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
