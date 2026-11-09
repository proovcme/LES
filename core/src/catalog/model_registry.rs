//! Append-only model registry. All mutations, CAS and audit records share one transaction.
use super::{boolean, first, rows, text, trim, Args, Failure, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const FIELDS: &[&str] = &[
    "display_name",
    "base_url",
    "model_id",
    "locality",
    "requested_context_tokens",
    "secret_ref",
    "extension_type",
    "enabled",
];
fn domain(code: &str) -> Failure {
    Failure::new("MODEL_REGISTRY_ERROR", code)
}
fn conflict(code: &str) -> Failure {
    Failure::new("MODEL_REGISTRY_CONFLICT", code)
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
fn id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| Failure::new("WORKSPACE_UNAVAILABLE", "Secure identifier unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn required(value: &Value, key: &str) -> Result<String> {
    let value = value[key]
        .as_str()
        .map(trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Failure::invalid(&format!("{key} must be non-empty")))?;
    Ok(value.to_string())
}
fn normalize_revision(row: &mut Value) -> Result<()> {
    for key in [
        "connection_id",
        "revision_id",
        "display_name",
        "base_url",
        "model_id",
        "created_at",
        "created_by",
    ] {
        row[key] = required(row, key)?.into();
    }
    if !matches!(
        row["locality"].as_str(),
        Some("loopback" | "private_network" | "remote")
    ) {
        return Err(Failure::invalid("Invalid connection locality"));
    }
    if row["protocol"] != "openai_compatible" || row["revision_no"].as_i64().is_none_or(|n| n < 1) {
        return Err(Failure::invalid("Invalid connection revision"));
    }
    if !row["requested_context_tokens"].is_null()
        && row["requested_context_tokens"]
            .as_i64()
            .is_none_or(|n| n < 1)
    {
        return Err(Failure::invalid(
            "requested_context_tokens must be positive",
        ));
    }
    for key in ["secret_ref", "extension_type"] {
        if !row[key].is_null() {
            row[key] = required(row, key)?.into();
        }
    }
    if !row["enabled"].is_boolean() {
        return Err(Failure::invalid("Invalid enabled flag"));
    }
    Ok(())
}
fn head(conn: &Connection, connection_id: &str) -> Result<Value> {
    first(conn, "SELECT r.* FROM les_model_connection_heads h JOIN les_model_connection_revisions r ON r.revision_id=h.revision_id WHERE h.connection_id=?", [connection_id])
}
fn revision(conn: &Connection, revision_id: &str) -> Result<Value> {
    first(
        conn,
        "SELECT * FROM les_model_connection_revisions WHERE revision_id=?",
        [revision_id],
    )
}
fn existing(row: Value, code: &str) -> Result<Value> {
    if row.is_null() {
        Err(domain(code))
    } else {
        Ok(row)
    }
}
fn available_name(conn: &Connection, name: &str, exclude: Option<&str>) -> Result<()> {
    if !first(conn, "SELECT r.connection_id FROM les_model_connection_heads h JOIN les_model_connection_revisions r ON r.revision_id=h.revision_id WHERE r.enabled=1 AND trim(r.display_name)=trim(?1) COLLATE NOCASE AND (?2 IS NULL OR r.connection_id<>?2) LIMIT 1", params![name, exclude])?.is_null() {
        return Err(domain("DISPLAY_NAME_IN_USE"));
    }
    Ok(())
}
fn insert_revision(conn: &Connection, row: &Value) -> Result<()> {
    conn.execute("INSERT INTO les_model_connection_revisions (revision_id,connection_id,revision_no,display_name,protocol,base_url,model_id,locality,requested_context_tokens,secret_ref,extension_type,enabled,created_at,created_by) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![row["revision_id"].as_str(),row["connection_id"].as_str(),row["revision_no"].as_i64(),row["display_name"].as_str(),row["protocol"].as_str(),row["base_url"].as_str(),row["model_id"].as_str(),row["locality"].as_str(),row["requested_context_tokens"].as_i64(),row["secret_ref"].as_str(),row["extension_type"].as_str(),row["enabled"].as_bool(),row["created_at"].as_str(),row["created_by"].as_str()])?;
    Ok(())
}
fn audit(
    conn: &Connection,
    action: &str,
    actor: &str,
    connection: Option<&str>,
    revision: Option<&str>,
    details: Value,
) -> Result<()> {
    conn.execute("INSERT INTO les_model_connection_audit (event_id,action,connection_id,revision_id,actor,created_at,details_json) VALUES (?,?,?,?,?,?,?)", params![id()?, action, connection, revision, actor, now(), details.to_string()])?;
    Ok(())
}
fn role(args: &Args) -> Result<&str> {
    let value = text(args, "role")?;
    if !matches!(value, "answer" | "embeddings" | "local_fallback") {
        return Err(Failure::invalid("Invalid connection role"));
    }
    Ok(value)
}
fn binding(conn: &Connection, role: &str) -> Result<Value> {
    first(
        conn,
        "SELECT * FROM les_model_role_bindings WHERE role=? ORDER BY binding_revision DESC LIMIT 1",
        [role],
    )
}
fn snapshot_date(value: &Value, key: &str) -> Result<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(&required(value, key)?)
        .map_err(|_| Failure::invalid(&format!("Invalid {key} timestamp")))
}
fn validate_snapshot(value: &Value) -> Result<()> {
    required(value, "snapshot_id")?;
    required(value, "connection_revision_id")?;
    if snapshot_date(value, "expires_at")? <= snapshot_date(value, "observed_at")? {
        return Err(Failure::invalid("expires_at must be after observed_at"));
    }
    let observations = value["observations"]
        .as_array()
        .ok_or_else(|| Failure::invalid("Invalid observations"))?;
    let mut seen = BTreeSet::new();
    for item in observations {
        let capability = required(item, "capability")?;
        if !matches!(
            capability.as_str(),
            "chat_completions"
                | "streaming"
                | "tools"
                | "structured_output"
                | "responses"
                | "embeddings"
                | "models"
                | "token_count"
                | "rerank"
        ) || !seen.insert(capability)
        {
            return Err(Failure::invalid(
                "Invalid or duplicate capability observation",
            ));
        }
        if !matches!(
            item["state"].as_str(),
            Some("supported" | "unsupported" | "unknown")
        ) || !matches!(
            item["evidence_source"].as_str(),
            Some("probe" | "operator_declaration" | "template_default" | "unavailable")
        ) {
            return Err(Failure::invalid("Invalid capability evidence"));
        }
        snapshot_date(item, "observed_at")?;
        if item.get("detail").is_some_and(|v| !v.is_string()) {
            return Err(Failure::invalid("Invalid observation detail"));
        }
    }
    let options = value["transport_options"]
        .as_object()
        .ok_or_else(|| Failure::invalid("Invalid transport options"))?;
    if options.values().any(|v| !v.is_string()) {
        return Err(Failure::invalid("Invalid transport option value"));
    }
    for (key, allowed) in [
        (
            "max_output_field",
            &["max_tokens", "max_completion_tokens"][..],
        ),
        ("chat_protocol", &["openai_chat_v1", "native_chat_v1"][..]),
    ] {
        if options
            .get(key)
            .is_some_and(|v| !allowed.contains(&v.as_str().unwrap_or("")))
        {
            return Err(Failure::invalid(&format!("unsupported {key}")));
        }
    }
    Ok(())
}

/// The HTTP settings owner checks the head and binding inside the write transaction.
/// Secrets are kept in the established local store, never in arguments/audit rows.
pub fn manage(
    database: &std::path::Path,
    env_path: &std::path::Path,
    action: &str,
    args: &Args,
    secret: Option<&str>,
) -> Result<Value> {
    let mut conn =
        super::connection(database, true)?.ok_or_else(|| domain("MODEL_REGISTRY_UNAVAILABLE"))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    execute(&tx, "model_registry_init", &Args::new())?;
    let actor = text(args, "actor")?;
    let mut secret_write = None;
    let value = match action {
        "create" => {
            let current = execute(&tx, "model_registry_create", args)?;
            if secret.is_some() {
                let id = current["connection_id"].as_str().unwrap();
                let reference = owned_reference(id);
                let next = revision_change(&tx, &current, actor, json!({"secret_ref":reference}))?;
                secret_write = Some(reference);
                next
            } else {
                current
            }
        }
        "revise" | "disable" | "secret" => {
            let id = text(args, "connection_id")?;
            let mut current = existing(head(&tx, id)?, "CONNECTION_NOT_FOUND")?;
            if current["revision_id"] != text(args, "expected_revision_id")? {
                return Err(conflict("CONNECTION_REVISION_CONFLICT"));
            }
            match action {
                "disable" => {
                    if args.get("confirm_bound_roles").and_then(Value::as_bool) != Some(true) {
                        for role in ["answer", "embeddings", "local_fallback"] {
                            let active = binding(&tx, role)?;
                            if let Some(id) = active["connection_revision_id"].as_str() {
                                if revision(&tx, id)?["connection_id"] == current["connection_id"] {
                                    return Err(conflict("BOUND_CONNECTION_CONFIRMATION_REQUIRED"));
                                }
                            }
                        }
                    }
                    revision_change(&tx, &current, actor, json!({"enabled":false}))?
                }
                "revise" => {
                    let changes = &args["changes"];
                    if current["secret_ref"].is_string()
                        && ["base_url", "locality"]
                            .iter()
                            .any(|key| changes.get(*key).is_some_and(|v| v != &current[*key]))
                    {
                        return Err(conflict("CONNECTION_SECRET_ENDPOINT_CHANGED"));
                    }
                    revision_change(&tx, &current, actor, changes.clone())?
                }
                _ => {
                    let reference = current["secret_ref"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| owned_reference(id));
                    if current["secret_ref"].is_null() {
                        current =
                            revision_change(&tx, &current, actor, json!({"secret_ref":reference}))?;
                    }
                    secret_write = Some(reference);
                    current
                }
            }
        }
        "bind" => {
            let role = role(args)?;
            let revision_id = text(args, "connection_revision_id")?;
            let target = existing(revision(&tx, revision_id)?, "CONNECTION_REVISION_NOT_FOUND")?;
            let current = head(&tx, target["connection_id"].as_str().unwrap())?;
            if current["revision_id"] != revision_id || current["enabled"] != 1 {
                return Err(conflict("CONNECTION_REVISION_CONFLICT"));
            }
            if role == "local_fallback" && target["locality"] == "remote" {
                return Err(domain("LOCAL_FALLBACK_REQUIRES_LOCAL_MODEL"));
            }
            let snapshot = execute(
                &tx,
                "model_registry_snapshot",
                json!({"connection_revision_id":revision_id})
                    .as_object()
                    .unwrap(),
            )?;
            let observations = snapshot["observations_json"]
                .as_str()
                .and_then(|v| serde_json::from_str::<Value>(v).ok())
                .unwrap_or(Value::Null);
            let required = if role == "embeddings" {
                "embeddings"
            } else {
                "chat_completions"
            };
            crate::model_capabilities::require(
                &json!({"expires_at":snapshot["expires_at"],"observations":observations}),
                &[required.to_owned()],
                None,
            )
            .map_err(|code| domain(code.split(':').next().unwrap_or("CAPABILITY_REQUIRED")))?;
            execute(&tx, "model_registry_bind", args)?
        }
        _ => return Err(Failure::invalid("Unknown model settings action")),
    };
    if let Some(reference) = secret_write {
        let secret = secret
            .filter(|v| !v.chars().any(char::is_control))
            .ok_or_else(|| domain("SECRET_VALUE_INVALID"))?;
        crate::model_secrets::replace(env_path, &reference, secret, actor).map_err(domain)?;
    }
    tx.commit()?;
    Ok(value)
}

fn owned_reference(id: &str) -> String {
    let token = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("env:LES_MODEL_CONNECTION_{token}_API_KEY")
}

pub fn record_probe(
    database: &std::path::Path,
    revision: &str,
    actor: &str,
    mut observations: Vec<Value>,
    mut options: Value,
) -> Result<Value> {
    let mut db =
        super::connection(database, true)?.ok_or_else(|| domain("MODEL_REGISTRY_UNAVAILABLE"))?;
    db.pragma_update(None, "foreign_keys", true)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let clock = chrono::Utc::now();
    let when = clock.to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
    let mut expires = clock + chrono::Duration::hours(24);
    let prior = execute(
        &tx,
        "model_registry_snapshot",
        json!({"connection_revision_id":revision})
            .as_object()
            .unwrap(),
    )?;
    let expiry = prior["expires_at"]
        .as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&chrono::Utc))
        .filter(|v| *v > clock);
    let previous = prior["observations_json"]
        .as_str()
        .and_then(|v| serde_json::from_str::<Vec<Value>>(v).ok())
        .unwrap_or_default();
    let old_options = prior["transport_options_json"]
        .as_str()
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
        .unwrap_or(Value::Null);
    for row in &mut observations {
        if row["detail"] == "not_requested" {
            if let (Some(expiry), Some(old)) = (
                expiry,
                previous
                    .iter()
                    .find(|v| v["capability"] == row["capability"] && v["state"] == "supported"),
            ) {
                let keys: &[&str] = match row["capability"].as_str() {
                    Some("chat_completions") => &[
                        "chat_protocol",
                        "observed_context_tokens",
                        "max_output_field",
                    ],
                    Some("embeddings") => &["embedding_protocol"],
                    _ => &[],
                };
                for key in keys {
                    if old_options[*key].is_string() {
                        options[*key] = old_options[*key].clone();
                    }
                }
                *row = old.clone();
                expires = expires.min(expiry);
                continue;
            }
        }
        row["observed_at"] = json!(when);
    }
    let expires = expires.to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
    let snapshot = json!({"snapshot_id":format!("cap:{}",id()?), "connection_revision_id":revision,
        "observed_at":when,"expires_at":expires,"observations":observations,"transport_options":options});
    execute(
        &tx,
        "model_registry_save_snapshot",
        json!({"snapshot":snapshot,"actor":actor})
            .as_object()
            .unwrap(),
    )?;
    tx.commit()?;
    Ok(snapshot)
}

fn revision_change(
    conn: &Connection,
    current: &Value,
    actor: &str,
    changes: Value,
) -> Result<Value> {
    let args = json!({"connection_id":current["connection_id"],"expected_revision_id":current["revision_id"],
        "actor":actor,"changes":changes});
    execute(conn, "model_registry_revise", args.as_object().unwrap())
}
pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    match action {
        "model_registry_view" => {
            let mut connections = rows(conn,"SELECT r.* FROM les_model_connection_heads h JOIN les_model_connection_revisions r ON r.revision_id=h.revision_id ORDER BY lower(r.display_name),r.connection_id",[])?;
            for row in &mut connections {
                row["snapshot"] = first(conn,"SELECT * FROM les_model_capability_snapshots WHERE connection_revision_id=? ORDER BY observed_at DESC,rowid DESC LIMIT 1",[row["revision_id"].as_str()])?;
            }
            let mut bindings=serde_json::Map::new();
            for role in ["answer","embeddings","local_fallback"] { bindings.insert(role.to_string(),binding(conn,role)?); }
            Ok(json!({"connections":connections,"bindings":bindings}))
        }
        "model_registry_init" => { conn.execute_batch(include_str!("model_registry.sql"))?; Ok(Value::Null) }
        "model_registry_get" => existing(head(conn,text(args,"connection_id")?)?,"CONNECTION_NOT_FOUND"),
        "model_registry_revision" => existing(revision(conn,text(args,"revision_id")?)?,"CONNECTION_REVISION_NOT_FOUND"),
        "model_registry_list" => Ok(json!(rows(conn,"SELECT r.* FROM les_model_connection_heads h JOIN les_model_connection_revisions r ON r.revision_id=h.revision_id WHERE ?1 OR r.enabled=1 ORDER BY lower(r.display_name),r.connection_id", [boolean(args,"include_disabled",false)?])?)),
        "model_registry_revisions" => Ok(json!(rows(conn,"SELECT * FROM les_model_connection_revisions WHERE connection_id=? ORDER BY revision_no",[text(args,"connection_id")?])?)),
        "model_registry_binding" => binding(conn,role(args)?),
        "model_registry_snapshot" => first(conn,"SELECT * FROM les_model_capability_snapshots WHERE connection_revision_id=? ORDER BY observed_at DESC,rowid DESC LIMIT 1",[text(args,"connection_revision_id")?]),
        "model_registry_create" | "model_registry_import" => {
            let imported = action == "model_registry_import";
            let connection_id = if imported {
                let value = text(args,"stable_connection_id")?.trim();
                if !value.starts_with("legacy:") || value == "legacy:" { return Err(domain("LEGACY_CONNECTION_ID_INVALID")); }
                value.to_string()
            } else { format!("conn:{}",id()?) };
            let mut row = Value::Object(args.clone());
            row["connection_id"]=json!(connection_id); row["revision_id"]=json!(format!("{connection_id}:r1"));
            row["revision_no"]=json!(1); row["protocol"]=json!("openai_compatible"); row["enabled"]=json!(true);
            row["created_at"]=json!(now()); row["created_by"]=json!(text(args,"actor")?);
            normalize_revision(&mut row)?;
            if imported { let current = head(conn,&connection_id)?; if !current.is_null() { return Ok(current); } }
            available_name(conn,row["display_name"].as_str().unwrap(),None)?;
            insert_revision(conn,&row)?;
            conn.execute("INSERT INTO les_model_connection_heads (connection_id,revision_id) VALUES (?,?)",params![connection_id,row["revision_id"].as_str()])?;
            audit(conn,if imported {"legacy_connection_imported"} else {"connection_created"},row["created_by"].as_str().unwrap(),Some(&connection_id),row["revision_id"].as_str(),json!({"revision_no":1}))?;
            Ok(row)
        }
        "model_registry_revise" => {
            let changes = args.get("changes").and_then(Value::as_object).ok_or_else(|| Failure::invalid("Invalid connection changes"))?;
            let unknown: Vec<_> = changes.keys().filter(|k| !FIELDS.contains(&k.as_str())).cloned().collect();
            if !unknown.is_empty() { return Err(domain(&format!("UNSUPPORTED_CONNECTION_FIELDS: {}",unknown.join(",")))); }
            let connection_id = text(args,"connection_id")?;
            let expected = text(args,"expected_revision_id")?;
            let mut row = existing(head(conn,connection_id)?,"CONNECTION_NOT_FOUND")?;
            if row["revision_id"] != expected { return Err(conflict("CONNECTION_REVISION_CONFLICT")); }
            // SQLite stores booleans as integers; the native contract uses a boolean.
            row["enabled"]=json!(row["enabled"].as_i64()==Some(1));
            for (key,value) in changes { row[key]=value.clone(); }
            let number = row["revision_no"].as_i64().unwrap_or(0).checked_add(1).ok_or_else(|| Failure::invalid("Revision overflow"))?;
            row["revision_no"]=json!(number); row["revision_id"]=json!(format!("{connection_id}:r{number}"));
            row["created_at"]=json!(now()); row["created_by"]=json!(text(args,"actor")?);
            normalize_revision(&mut row)?;
            if row["enabled"]==true { available_name(conn,row["display_name"].as_str().unwrap(),Some(connection_id))?; }
            insert_revision(conn,&row)?;
            if conn.execute("UPDATE les_model_connection_heads SET revision_id=? WHERE connection_id=? AND revision_id=?",params![row["revision_id"].as_str(),connection_id,expected])? != 1 { return Err(conflict("CONNECTION_REVISION_CONFLICT")); }
            audit(conn,"connection_revised",row["created_by"].as_str().unwrap(),Some(connection_id),row["revision_id"].as_str(),json!({"revision_no":number,"fields":changes.keys().collect::<Vec<_>>()}))?;
            Ok(row)
        }
        "model_registry_bind" => {
            let role = role(args)?;
            let revision_id = text(args,"connection_revision_id")?;
            let target = existing(revision(conn,revision_id)?,"CONNECTION_REVISION_NOT_FOUND")?;
            if target["enabled"].as_i64()!=Some(1) { return Err(domain("CONNECTION_DISABLED")); }
            let current = binding(conn,role)?;
            let expected = args.get("expected_binding_revision").unwrap_or(&Value::Null);
            if !expected.is_null() && expected.as_i64().is_none_or(|n| n<1) { return Err(Failure::invalid("Invalid expected binding revision")); }
            if current["binding_revision"] != *expected { return Err(conflict("ROLE_BINDING_CONFLICT")); }
            let number = current["binding_revision"].as_i64().unwrap_or(0).checked_add(1).ok_or_else(|| Failure::invalid("Binding revision overflow"))?;
            let actor = required(&Value::Object(args.clone()),"actor")?;
            let timestamp = now();
            conn.execute("INSERT INTO les_model_role_bindings (role,binding_revision,connection_revision_id,bound_at,bound_by) VALUES (?,?,?,?,?)",params![role,number,revision_id,timestamp,actor])?;
            audit(conn,"role_bound",&actor,target["connection_id"].as_str(),Some(revision_id),json!({"role":role,"binding_revision":number}))?;
            binding(conn,role)
        }
        "model_registry_save_snapshot" => {
            let value = args.get("snapshot").ok_or_else(|| Failure::invalid("Invalid capability snapshot"))?;
            validate_snapshot(value)?;
            let revision_id = value["connection_revision_id"].as_str().unwrap();
            existing(revision(conn,revision_id)?,"CONNECTION_REVISION_NOT_FOUND")?;
            if !first(conn,"SELECT 1 FROM les_model_capability_snapshots WHERE snapshot_id=?",[value["snapshot_id"].as_str()])?.is_null() { return Err(domain("CAPABILITY_SNAPSHOT_EXISTS")); }
            let actor = text(args,"actor")?;
            conn.execute("INSERT INTO les_model_capability_snapshots (snapshot_id,connection_revision_id,observations_json,observed_at,expires_at,transport_options_json,created_by) VALUES (?,?,?,?,?,?,?)",params![value["snapshot_id"].as_str(),revision_id,value["observations"].to_string(),value["observed_at"].as_str(),value["expires_at"].as_str(),value["transport_options"].to_string(),actor])?;
            audit(conn,"capabilities_recorded",actor,None,Some(revision_id),json!({"snapshot_id":value["snapshot_id"]}))?;
            Ok(Value::Null)
        }
        _ => Err(Failure::invalid("Unknown model registry action")),
    }
}
