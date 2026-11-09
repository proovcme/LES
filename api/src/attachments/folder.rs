use super::*;

pub(super) fn reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
pub(super) async fn receive(
    context: Arc<Context>,
    request: Request,
    caller: String,
    key: Option<String>,
) -> Result<Value> {
    let bytes = to_bytes(request.into_body(), 65536)
        .await
        .map_err(|_| "ATTACHMENT_REQUEST_INVALID")?;
    let input: Value = serde_json::from_slice(&bytes).map_err(|_| "ATTACHMENT_REQUEST_INVALID")?;
    let path = input["path"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains('\0'))
        .ok_or("ATTACHMENT_REQUEST_INVALID")?
        .to_owned();
    let state = context.state_root.clone();
    let root = tokio::task::spawn_blocking(move || crate::intake::attachment_folder(&state, &path))
        .await
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")??;
    let _slot = tokio::time::timeout(
        Duration::from_secs(30),
        context.attachment_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?
    .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?;
    let mut value =
        worker(json!({"action":"folder","path":root,"owned_state":context.state_root})).await?;
    let directory = super::upload::scratch(&context).await?;
    let path = directory.join(format!(
        "native-{}.md",
        les_core::chat_attachments::new_id()?
    ));
    let label = format!(
        "{}.md",
        value["name"].as_str().ok_or("ATTACHMENT_READ_FAILED")?
    );
    let settings = policy(&context).await?;
    tokio::task::spawn_blocking(move || -> Result<Value> {
        use std::io::Write;
        // The guard outlives the owned file and the publication transaction.
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
        let _guard = super::upload::Temporary(path.clone());
        file.write_all(
            value["text"]
                .as_str()
                .ok_or("ATTACHMENT_READ_FAILED")?
                .as_bytes(),
        )
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
        drop(file);
        let claim =
            les_core::chat_attachments::claim(&path, &label, &caller, key.as_deref(), "folder")?;
        if let Some(replay) =
            les_core::chat_attachments::replay_upload(&settings.root, &claim, settings.age)?
        {
            return Ok(replay);
        }
        value["attachment_id"] = json!(claim.ident);
        les_core::chat_attachments::cleanup(&settings.root, settings.age)?;
        Ok(les_core::chat_attachments::save_upload(
            &settings.root,
            &path,
            &label,
            &claim,
            &value,
        )?)
    })
    .await
    .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE".to_owned())?
}
