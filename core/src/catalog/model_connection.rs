//! Read the execution binding and its capabilities in one SQLite snapshot.
use super::{first, text, Args, Failure, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

pub(super) fn snapshot(conn: &Connection, args: &Args) -> Result<Value> {
    let bound;
    let revision_id = match (args.get("role"), args.get("revision_id")) {
        (Some(role), None)
            if matches!(
                role.as_str(),
                Some("answer" | "embeddings" | "local_fallback")
            ) =>
        {
            bound = first(
                conn,
                "SELECT connection_revision_id FROM les_model_role_bindings WHERE role=?
                 ORDER BY binding_revision DESC LIMIT 1",
                [text(args, "role")?],
            )?;
            let Some(revision_id) = bound["connection_revision_id"].as_str() else {
                return Ok(Value::Null);
            };
            revision_id
        }
        (None, Some(_)) => text(args, "revision_id")?,
        _ => return Err(Failure::invalid("Invalid model execution selector")),
    };
    let revision = first(
        conn,
        "SELECT * FROM les_model_connection_revisions WHERE revision_id=?",
        [revision_id],
    )?;
    if revision.is_null() {
        return Ok(json!({"missing":"CONNECTION_REVISION_NOT_FOUND"}));
    }
    let head = first(
        conn,
        "SELECT r.enabled FROM les_model_connection_heads h
         JOIN les_model_connection_revisions r ON r.revision_id=h.revision_id
         WHERE h.connection_id=?",
        [revision["connection_id"]
            .as_str()
            .ok_or_else(|| Failure::invalid("Invalid model connection ID"))?],
    )?;
    let Some(enabled) = head["enabled"].as_i64() else {
        return Ok(json!({"missing":"CONNECTION_NOT_FOUND"}));
    };
    let snapshot = first(conn,
        "SELECT snapshot_id,connection_revision_id,observations_json,observed_at,expires_at,transport_options_json
         FROM les_model_capability_snapshots WHERE connection_revision_id=?
         ORDER BY observed_at DESC,rowid DESC LIMIT 1", [revision_id],
    )?;
    Ok(json!({"revision":revision,"enabled":enabled != 0,"snapshot":snapshot}))
}
