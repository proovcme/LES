use super::*;
use crate::index_batch::configuration::Settings;
use serde_json::{json, Value};

pub(super) fn consent(settings: &Settings) -> bool {
    ["true", "1", "yes", "on"].contains(
        &settings
            .get("LES_CLOUD_CONSENT", "false")
            .trim()
            .to_ascii_lowercase()
            .as_str(),
    )
}
pub(super) fn check(context: &Context, ids: &Value) -> Result<()> {
    if !consent(&Settings::load(&context.state_root)?) {
        return Err("CLOUD_CONSENT_REQUIRED".into());
    }
    les_core::chat_request::egress::datasets(&context.database, ids)?;
    Ok(())
}
pub(super) fn admit(
    context: &Context,
    request: &Value,
    scope: &Value,
    revision: &Value,
    caller: Option<&str>,
) -> Result<String> {
    if request["attachment_id"].is_string() || request["attachment_context"].is_string() {
        return Err("CLOUD_ATTACHMENT_LOCAL_ONLY".into());
    }
    if scope["warnings"].as_array().is_some_and(|v| !v.is_empty()) {
        return Err("CLOUD_SCOPE_UNVERIFIED".into());
    }
    let mut ids = scope["resolved_dataset_ids"]
        .as_array()
        .cloned()
        .ok_or("CLOUD_SCOPE_UNVERIFIED")?;
    ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    check(context, &json!(ids))?;
    Ok(les_core::sha256_hex(
        json!([
            "les.remote-context.v1",
            revision["revision_id"],
            ids,
            caller
        ])
        .to_string()
        .as_bytes(),
    ))
}
