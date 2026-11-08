//! Immutable prompt/skill/profile revisions and per-conversation snapshot ownership.
use crate::workspace::{Failure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path, sync::OnceLock};
mod writes;
const MODES: &[&str] = &["search", "agent"];
fn defaults() -> &'static Value {
    static VALUE: OnceLock<Value> = OnceLock::new();
    VALUE.get_or_init(|| {
        serde_json::from_str(include_str!("../../../config/chat-profile-defaults.json"))
            .expect("compiled profiles")
    })
}
fn invalid(text: impl Into<String>) -> Failure {
    Failure::new("PROFILE_INVALID", text)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| invalid(format!("Некорректное поле {key}")))
}
fn optional<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v[key].as_str().filter(|s| !s.is_empty())
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
fn trim(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}
fn uuid() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| invalid("Не удалось создать ID редакции"))?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let h = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
fn table(kind: &str) -> Result<&'static str> {
    match kind {
        "prompt" => Ok("les_prompt_revisions"),
        "skill" => Ok("les_skill_revisions"),
        "profile" => Ok("les_profile_revisions"),
        _ => Err(invalid(format!("Неизвестный тип редакции: {kind}"))),
    }
}
pub fn mode(value: &str) -> Result<&'static str> {
    match crate::unicode::casefold(trim(value)).as_str() {
        "search" | "rag" => Ok("search"),
        "engineer" | "review" | "doc_review" | "normcontrol" | "estimator" | "smeta"
        | "smeta_harness" => Err(invalid("Этот режим отсутствует в LES RAG")),
        _ => Ok("agent"),
    }
}
fn label(mode: &str) -> &'static str {
    if mode == "search" {
        "Поиск"
    } else {
        "Агент"
    }
}
fn bounded(value: &Value, default: i64, low: i64, high: i64) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_bool().map(i64::from))
        .or_else(|| crate::unicode::saturated_integer(value.as_str()?))
        .or_else(|| {
            value
                .is_number()
                .then(|| value.to_string())
                .as_deref()
                .and_then(crate::unicode::saturated_integer)
        })
        .or_else(|| value.as_f64().filter(|v| v.is_finite()).map(|v| v as i64))
        .unwrap_or(default)
        .clamp(low, high)
}
pub fn retrieval_policy(snapshot: &Value) -> Value {
    let policy = &snapshot["rag_policy"];
    let evidence = bounded(&policy["model_evidence_k"], 6, 1, 64);
    json!({"model_evidence_k":evidence,"retrieval_candidate_k":bounded(&policy["retrieval_candidate_k"],64,1,512).max(evidence),
        "document_diversity_k":bounded(&policy["document_diversity_k"],2,1,32)})
}
pub fn effective(snapshot: &Value) -> Result<Value> {
    let mut value = snapshot
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Snapshot профиля повреждён"))?;
    let mut policy = snapshot["rag_policy"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    policy.extend(retrieval_policy(snapshot).as_object().unwrap().clone());
    policy.remove("model_authored_initial_query");
    policy.remove("system_datasets");
    value.insert("rag_policy".into(), policy.into());
    let mut tools = snapshot["tools"].as_array().cloned().unwrap_or_default();
    if let Some(index) = tools.iter().position(|v| v == "web_search") {
        if !tools.iter().any(|v| v == "web_read") {
            tools.insert(index + 1, json!("web_read"));
            value.insert("tools".into(), json!(tools));
            let mut bridges = snapshot["effective_capability_bridges"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !bridges.iter().any(|v| v == "paired_web_capability_v1") {
                bridges.push(json!("paired_web_capability_v1"));
            }
            value.insert("effective_capability_bridges".into(), json!(bridges));
        }
    }
    Ok(value.into())
}
fn validated(kind: &str, value: &str) -> Result<String> {
    let limit = match kind {
        "prompt" => 16000,
        "skill" => 8000,
        _ => return Err(invalid(format!("Неизвестный тип редакции: {kind}"))),
    };
    let value = trim(value);
    if value.is_empty() {
        return Err(invalid("Текст редакции не может быть пустым"));
    }
    let length = value.chars().count();
    if length > limit {
        return Err(invalid(format!(
            "profile_text_too_long: {kind} содержит {length} символов при лимите {limit}"
        )));
    }
    Ok(value.into())
}
fn snapshot(
    base: &Value,
    id: &str,
    mode: &str,
    name: &str,
    revision: i64,
    prompt_id: &str,
    skill_id: &str,
    created: &str,
    factory: bool,
) -> Value {
    json!({"schema":"les.chat_profile_snapshot.v1","revision_id":id,"mode":mode,"label":label(mode),"name":name,
        "revision_no":revision,"prompt_revision_id":prompt_id,"prompt_text":base["prompt"],
        "prompt_sha256":crate::sha256_hex(base["prompt"].as_str().unwrap_or("").as_bytes()),
        "skill_revision_id":skill_id,"skill_text":base["skill"],
        "skill_sha256":crate::sha256_hex(base["skill"].as_str().unwrap_or("").as_bytes()),
        "tools":base["tools"],"model_policy":base["model_policy"],"rag_policy":base["rag_policy"],
        "is_factory":factory,"created_at":created})
}
fn seed(conn: &Connection, factory: &Value) -> Result<()> {
    conn.execute_batch(include_str!("schema.sql"))?;
    for &mode in MODES {
        let mut contract = factory[mode].clone();
        contract["prompt"] = json!(trim(text(&contract, "prompt")?));
        contract["skill"] = json!(trim(text(&contract, "skill")?));
        let prompt = text(&contract, "prompt")?;
        let skill = text(&contract, "skill")?;
        let id = format!(
            "factory:profile:{mode}:{}",
            crate::sha256_hex(contract.to_string().as_bytes())
        );
        let prompt_id = format!(
            "factory:prompt:{mode}:{}",
            crate::sha256_hex(prompt.as_bytes())
        );
        let skill_id = format!(
            "factory:skill:{mode}:{}",
            crate::sha256_hex(skill.as_bytes())
        );
        for (table, id) in [
            ("les_profile_revisions", &id),
            ("les_prompt_revisions", &prompt_id),
            ("les_skill_revisions", &skill_id),
        ] {
            if conn
                .query_row(
                    &format!("SELECT is_factory FROM {table} WHERE revision_id=?"),
                    [id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .is_some_and(|v| v == 0)
            {
                return Err(invalid(format!("Зарезервированный factory ID занят: {id}")));
            }
        }
        let old = conn
            .query_row(
                "SELECT revision_no,created_at FROM les_profile_revisions WHERE revision_id=?",
                [&id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        let created = if let Some((_, created)) = old {
            created
        } else {
            let revision=conn.query_row("SELECT COALESCE(MAX(revision_no),0)+1 FROM les_profile_revisions WHERE mode=? AND is_factory=1",[mode],|r|r.get::<_,i64>(0))?;
            let created = now();
            let name = format!("{} · Base", label(mode));
            for (table, id, value) in [
                ("les_prompt_revisions", &prompt_id, prompt),
                ("les_skill_revisions", &skill_id, skill),
            ] {
                conn.execute(&format!("INSERT OR IGNORE INTO {table} (revision_id,name,revision_no,text_value,sha256,is_factory,created_at) VALUES(?,?,?,?,?,1,?)"),params![id,name,revision,value,crate::sha256_hex(value.as_bytes()),created])?;
            }
            let value = snapshot(
                &contract, &id, mode, &name, revision, &prompt_id, &skill_id, &created, true,
            );
            conn.execute("INSERT OR IGNORE INTO les_profile_revisions (revision_id,mode,name,revision_no,prompt_revision_id,skill_revision_id,tools_json,model_policy_json,rag_policy_json,snapshot_json,is_factory,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,1,?)",
                params![id,mode,name,revision,prompt_id,skill_id,value["tools"].to_string(),value["model_policy"].to_string(),value["rag_policy"].to_string(),value.to_string(),created])?;
            created
        };
        conn.execute("INSERT OR IGNORE INTO les_active_profiles(mode,profile_revision_id,updated_at) VALUES(?,?,?)",params![mode,id,created])?;
    }
    Ok(())
}
fn read_snapshot(conn: &Connection, id: &str) -> Result<Value> {
    let raw=conn.query_row("SELECT snapshot_json FROM les_profile_revisions WHERE revision_id=? AND deleted_at IS NULL",[id],|r|r.get::<_,String>(0)).optional()?.ok_or_else(||invalid("Версия профиля не найдена"))?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|_| invalid("Snapshot профиля повреждён"))?;
    if !value["prompt_text"].as_str().is_some_and(|v| !v.is_empty())
        || !value["skill_text"].as_str().is_some_and(|v| !v.is_empty())
    {
        return Err(invalid("Snapshot профиля повреждён"));
    }
    effective(&value)
}
fn resolve(conn: &Connection, args: &Value) -> Result<Value> {
    let mode = mode(args["requested_mode"].as_str().unwrap_or(""))?;
    let sid = trim(args["session_id"].as_str().unwrap_or(""));
    if !sid.is_empty() && args["apply_revision"] != true {
        let raw = conn
            .query_row(
                "SELECT snapshot_json FROM les_chat_profile_bindings WHERE session_id=? AND mode=?",
                params![sid, mode],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(value) = raw.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            if value["prompt_text"].as_str().is_some_and(|v| !v.is_empty())
                && value["skill_text"].as_str().is_some_and(|v| !v.is_empty())
            {
                return effective(&value);
            }
        }
    }
    let id = if let Some(id) = optional(args, "requested_revision_id")
        .map(trim)
        .filter(|s| !s.is_empty())
    {
        id.to_owned()
    } else {
        conn.query_row(
            "SELECT profile_revision_id FROM les_active_profiles WHERE mode=?",
            [mode],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| invalid(format!("Для профиля {mode} не назначена активная версия")))?
    };
    let value = read_snapshot(conn, &id)?;
    if value["mode"] != mode {
        return Err(invalid("Выбранная версия относится другому режиму"));
    }
    if !sid.is_empty() {
        let time = now();
        conn.execute("INSERT INTO les_chat_profile_bindings(session_id,mode,profile_revision_id,snapshot_json,bound_at,updated_at) VALUES(?,?,?,?,?,?) ON CONFLICT(session_id) DO UPDATE SET mode=excluded.mode,profile_revision_id=excluded.profile_revision_id,snapshot_json=excluded.snapshot_json,updated_at=excluded.updated_at",
            params![sid,mode,id,value.to_string(),time,time])?;
    }
    effective(&value)
}
fn text_revision(conn: &Connection, kind: &str, id: &str, require_live: bool) -> Result<Value> {
    let table = table(kind)?;
    let condition = if require_live {
        " AND deleted_at IS NULL"
    } else {
        ""
    };
    conn.query_row(&format!("SELECT revision_id,name,revision_no,text_value,sha256,is_factory,source_revision_id,created_at FROM {table} WHERE revision_id=?{condition}"),[id],|r|Ok(json!({
        "revision_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"revision_no":r.get::<_,i64>(2)?,"text":r.get::<_,String>(3)?,
        "sha256":r.get::<_,String>(4)?,"is_factory":r.get::<_,i64>(5)?!=0,"source_revision_id":r.get::<_,Option<String>>(6)?,"created_at":r.get::<_,String>(7)?})))
        .optional()?.ok_or_else(||invalid(format!("Редакция {kind} не найдена: {id}")))
}
fn registry(conn: &Connection, request: &Value) -> Result<Value> {
    let mut profiles = Vec::new();
    for &mode in MODES {
        let active = conn
            .query_row(
                "SELECT profile_revision_id FROM les_active_profiles WHERE mode=?",
                [mode],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default();
        let mut statement=conn.prepare("SELECT snapshot_json FROM les_profile_revisions WHERE mode=? AND deleted_at IS NULL ORDER BY is_factory DESC,revision_no,created_at")?;
        let raw = statement
            .query_map([mode], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let revisions = raw
            .iter()
            .map(|s| {
                serde_json::from_str::<Value>(s)
                    .map_err(|_| invalid("Snapshot профиля повреждён"))
                    .and_then(|v| effective(&v))
            })
            .collect::<Result<Vec<_>>>()?;
        let selected = revisions
            .iter()
            .find(|v| v["revision_id"] == active)
            .cloned()
            .unwrap_or(json!({}));
        profiles.push(json!({"mode":mode,"label":label(mode),"active_revision_id":active,"active":selected,"revisions":revisions}));
    }
    let texts = |kind: &str| -> Result<Vec<Value>> {
        let table = table(kind)?;
        let mut statement=conn.prepare(&format!("SELECT revision_id FROM {table} WHERE deleted_at IS NULL ORDER BY is_factory DESC,revision_no,created_at"))?;
        let ids = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| text_revision(conn, kind, id, true))
            .collect()
    };
    Ok(
        json!({"schema":"les.chat_profiles.v1","default_mode":"agent","text_limits":{"prompt":16000,"skill":8000},
        "profiles":profiles,"prompt_revisions":texts("prompt")?,"skill_revisions":texts("skill")?,
        "tools":request.get("registry_tools").cloned().unwrap_or(json!([]))}),
    )
}
pub fn execute(request: &Value) -> Value {
    match run(request) {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(e) => {
            json!({"status":"failed","code":e.code,"detail":e.detail,"sqlite_extended":e.sqlite_extended})
        }
    }
}
pub fn instructions(snapshot: &Value) -> Result<String> {
    let mut parts = vec![trim(
        snapshot["prompt_text"]
            .as_str()
            .ok_or_else(|| invalid("Snapshot профиля повреждён"))?,
    )
    .to_owned()];
    if let Some(skill) = snapshot["skill_text"]
        .as_str()
        .map(trim)
        .filter(|v| !v.is_empty())
    {
        parts.push(format!("Инструкции выбранного профиля:\n{skill}"));
    }
    if let Some(skills) = snapshot["additional_skills"].as_array() {
        for item in skills {
            parts.push(text(item, "text")?.to_owned());
        }
    }
    parts.push("Ссылки на материалы с метками [Источник N] оформляй точно этими метками; для веб-страниц используй предоставленный URL.".into());
    Ok(parts.join("\n\n"))
}
fn run(request: &Value) -> Result<Value> {
    if request["schema"] != "les.chat-profiles.v1" {
        return Err(invalid("Некорректный контракт профилей"));
    }
    let action = text(request, "action")?;
    let args = &request["arguments"];
    match action {
        "mode" => return Ok(json!(mode(args["mode"].as_str().unwrap_or(""))?)),
        "effective" => return effective(&args["snapshot"]),
        "retrieval_policy" => return Ok(retrieval_policy(&args["snapshot"])),
        "validate_text" => return Ok(json!(validated(text(args, "kind")?, text(args, "text")?)?)),
        "factory" => return Ok(defaults().clone()),
        _ => {}
    }
    if !matches!(
        action,
        "registry"
            | "resolve"
            | "snapshot"
            | "publish_text"
            | "publish_profile"
            | "activate"
            | "delete"
    ) {
        return Err(invalid("Неизвестное действие профиля"));
    }
    let path = Path::new(text(request, "database")?);
    if !path.is_absolute() {
        return Err(invalid("Некорректный путь базы профилей"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| invalid("Не удалось открыть базу профилей"))?;
    }
    let mut conn = crate::workspace::connection(path, true)?
        .ok_or_else(|| invalid("Не удалось открыть базу профилей"))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    seed(&tx, request.get("factory").unwrap_or(defaults()))?;
    let mut owned = request.clone();
    if matches!(action, "registry" | "publish_profile")
        && (!owned["registry_tools"].is_array() || !owned["known_tools"].is_array())
    {
        let registry = crate::tool_registry::from_connection(Some(&tx), None)?;
        if !owned["registry_tools"].is_array() {
            owned["registry_tools"] = registry["tools"].clone();
        }
        if !owned["known_tools"].is_array() {
            owned["known_tools"] = json!(registry["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["name"].clone())
                .collect::<Vec<_>>());
        }
    }
    let value = match action {
        "registry" => registry(&tx, &owned)?,
        "resolve" => resolve(&tx, args)?,
        "snapshot" => read_snapshot(&tx, text(args, "revision_id")?)?,
        _ => writes::execute(&tx, action, args, &owned)?,
    };
    tx.commit()?;
    Ok(value)
}
