//! Scoped review ledgers. Only delivered source spans can support a saved conclusion.
mod binding;
mod quotes;
mod record;
mod source;
use crate::document_tasks::{digest, encode, number, parse, text};
use crate::workspace::{Failure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};
pub(crate) fn run(req: &Value, action: &str) -> Result<Value> {
    let r = &req["arguments"];
    match action {
        "context" => return Ok(binding::context(&r["state"])),
        "contract" => {
            return Ok(serde_json::from_str(include_str!(
                "../../../config/document-review-tool.json"
            ))
            .expect("checked review contract"))
        }
        "original" => return source::original(r),
        "selector" => return source::selector(&r["selector"], &r["allowed"], &r["files"]),
        "fragment" => return source::fragment(&r["snapshot"], r),
        "snapshot" => return source::snapshot(r),
        _ => {}
    }
    let path = Path::new(text(req, "database")?);
    if !path.is_absolute() {
        return Err(Failure::invalid(
            "Требуется абсолютный путь к базе проверок",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| Failure::invalid("Не удалось открыть базу проверок"))?;
    }
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(15))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS review_tasks(id TEXT PRIMARY KEY,owner TEXT NOT NULL,identity TEXT UNIQUE NOT NULL,question TEXT NOT NULL,units TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 0);
 CREATE TABLE IF NOT EXISTS review_events(task_id TEXT NOT NULL REFERENCES review_tasks(id),unit_id TEXT NOT NULL,revision INTEGER NOT NULL,fingerprint TEXT NOT NULL,observation TEXT NOT NULL,PRIMARY KEY(task_id,revision));
 CREATE INDEX IF NOT EXISTS review_latest ON review_events(task_id,unit_id,revision);
 CREATE TABLE IF NOT EXISTS review_reads(task_id TEXT NOT NULL REFERENCES review_tasks(id),unit_id TEXT NOT NULL,fingerprint TEXT NOT NULL,fragment TEXT NOT NULL,start INTEGER NOT NULL,stop INTEGER NOT NULL,PRIMARY KEY(task_id,unit_id,fingerprint,fragment,start,stop));
 CREATE TABLE IF NOT EXISTS review_receipts(task_id TEXT NOT NULL REFERENCES review_tasks(id),request_id TEXT NOT NULL,digest TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(task_id,request_id));")?;
    let value = operate(&tx, action, r)?;
    tx.commit()?;
    Ok(value)
}
fn task(conn: &Connection, owner: &str, id: &str) -> Result<Value> {
    let mut task=conn.query_row("SELECT id,owner,identity,question,units,revision FROM review_tasks WHERE owner=? AND id=?",params![owner,id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"owner":r.get::<_,String>(1)?,"identity":r.get::<_,String>(2)?,"question":r.get::<_,String>(3)?,"units":r.get::<_,String>(4)?,"revision":r.get::<_,i64>(5)?}))).optional()?.ok_or_else(||Failure::invalid("Проверка недоступна в текущем диалоге и области документов"))?;
    task["units"] = parse(task["units"].as_str().unwrap())?;
    Ok(task)
}
fn events(conn: &Connection, id: &str, latest: bool, offset: i64) -> Result<Vec<Value>> {
    let sql = if latest {
        "SELECT task_id,unit_id,revision,fingerprint,observation FROM review_events WHERE task_id=?1 AND revision IN(SELECT MAX(revision)FROM review_events WHERE task_id=?1 GROUP BY unit_id)"
    } else {
        "SELECT task_id,unit_id,revision,fingerprint,observation FROM review_events WHERE task_id=?1 ORDER BY revision LIMIT 9 OFFSET ?2"
    };
    let mut stmt = conn.prepare(sql)?;
    let decode = |r: &rusqlite::Row<'_>| {
        Ok(
            json!({"task_id":r.get::<_,String>(0)?,"unit_id":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?,"fingerprint":r.get::<_,String>(3)?,"observation":r.get::<_,String>(4)?}),
        )
    };
    let mut rows = if latest {
        stmt.query_map([id], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map(params![id, offset], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for row in &mut rows {
        row["observation"] = parse(row["observation"].as_str().unwrap())?;
    }
    Ok(rows)
}
fn latest(conn: &Connection, id: &str) -> Result<HashMap<String, Value>> {
    Ok(events(conn, id, true, 0)?
        .into_iter()
        .map(|r| (r["unit_id"].as_str().unwrap().to_owned(), r))
        .collect())
}
fn evaluate(
    id: &str,
    events: &HashMap<String, Value>,
    fp: &Value,
    visiting: &mut HashSet<String>,
    memo: &mut HashMap<String, String>,
) -> String {
    if let Some(saved) = memo.get(id) {
        return saved.clone();
    }
    let state = if let Some(event) = events.get(id) {
        if visiting.contains(id)
            || fp[id].as_str().is_none_or(|s| s.is_empty())
            || event["fingerprint"] != fp[id]
        {
            "needs_recheck".to_owned()
        } else {
            visiting.insert(id.to_owned());
            let stale = event["observation"]["dependencies"]
                .as_object()
                .into_iter()
                .flatten()
                .any(|(dep, rev)| {
                    events.get(dep).is_none_or(|r| r["revision"] != *rev)
                        || evaluate(dep, events, fp, visiting, memo) != "reviewed"
                });
            visiting.remove(id);
            if stale {
                "needs_recheck".into()
            } else {
                event["observation"]["status"]
                    .as_str()
                    .unwrap_or("needs_recheck")
                    .into()
            }
        }
    } else {
        "pending".into()
    };
    memo.insert(id.into(), state.clone());
    state
}
fn status(conn: &Connection, owner: &str, id: &str, fp: &Value) -> Result<Value> {
    let task = task(conn, owner, id)?;
    let events = latest(conn, id)?;
    let mut memo = HashMap::new();
    let mut units = task["units"]
        .as_array()
        .ok_or_else(|| Failure::invalid("Повреждён список проверки"))?
        .clone();
    let mut counts =
        json!({"pending":0,"reviewed":0,"needs_review":0,"excluded":0,"needs_recheck":0});
    for unit in &mut units {
        let id = text(unit, "unit_id")?.to_owned();
        let state = evaluate(&id, &events, fp, &mut HashSet::new(), &mut memo);
        let count = counts[&state]
            .as_u64()
            .ok_or_else(|| Failure::invalid("Повреждён статус проверки"))?;
        counts[&state] = json!(count + 1);
        unit["state"] = json!(state);
        unit["result_revision"] = events
            .get(&id)
            .map(|r| r["revision"].clone())
            .unwrap_or(Value::Null);
    }
    let next = ["needs_recheck", "pending"]
        .into_iter()
        .find_map(|s| {
            units
                .iter()
                .find(|u| u["state"] == s)
                .map(|u| u["unit_id"].clone())
        })
        .unwrap_or(Value::Null);
    let mut out = json!({"task_id":id,"revision":task["revision"],"total":units.len(),"units":units,"next_unit_id":next,"coverage_complete":counts["pending"]==0&&counts["needs_recheck"]==0,"needs_attention":counts["needs_review"]!=0||counts["needs_recheck"]!=0,"accepted":false,"scope":"explicit_inventory_only","conclusions_are_model_authored":true});
    out.as_object_mut()
        .unwrap()
        .extend(counts.as_object().unwrap().clone());
    Ok(out)
}
fn create(conn: &Connection, r: &Value) -> Result<Value> {
    let question = text(r, "question")?;
    if question.trim().is_empty() || question.chars().count() > 20000 {
        return Err(Failure::invalid("Нужна цель проверки до 20000 символов"));
    }
    let selectors = r["selectors"]
        .as_array()
        .filter(|s| !s.is_empty() && s.len() <= 32)
        .ok_or_else(|| Failure::invalid("Выберите от 1 до 32 документов или разделов"))?;
    let units = selectors
        .iter()
        .map(|s| Ok(json!({"unit_id":digest(s)?,"selector":s})))
        .collect::<Result<Vec<_>>>()?;
    if units
        .iter()
        .map(|u| u["unit_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>()
        .len()
        != units.len()
    {
        return Err(Failure::invalid(
            "Список проверки содержит повторные разделы",
        ));
    }
    let mut ordered = units.clone();
    ordered.sort_by(|a, b| a["unit_id"].as_str().cmp(&b["unit_id"].as_str()));
    let identity = digest(&json!([r["owner"], question, ordered]))?;
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM review_tasks WHERE identity=?",
            [&identity],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(json!(id));
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Failure::invalid("Не удалось создать проверку"))?;
    let id = random
        .iter()
        .map(|c| format!("{c:02x}"))
        .collect::<String>();
    conn.execute(
        "INSERT INTO review_tasks(id,owner,identity,question,units)VALUES(?,?,?,?,?)",
        params![
            id,
            text(r, "owner")?,
            identity,
            question,
            encode(&json!(units))?
        ],
    )?;
    Ok(json!(id))
}
fn operate(conn: &Connection, action: &str, r: &Value) -> Result<Value> {
    let owner = r["owner"].as_str().unwrap_or("");
    let id = r["task_id"].as_str().unwrap_or("");
    match action {
        "plan" => binding::plan(conn, r),
        "execute" => binding::execute(conn, r),
        "ack" => binding::ack(conn, r),
        "refresh" => {
            let mut state = r["state"].clone();
            let payload = binding::refresh(
                conn,
                &mut state,
                text(r, "task_id")?,
                &r["snapshots"],
                &r["errors"],
            )?;
            Ok(json!({"state":state,"payload":payload}))
        }
        "init" => Ok(Value::Null),
        "create" => create(conn, r),
        "task" => task(conn, owner, id),
        "resolve_reference" => {
            let q = text(r, "question")?;
            let regex = regex::Regex::new("[0-9a-fA-F]{32}").unwrap();
            let ids = regex
                .find_iter(q)
                .filter(|m| m.start() == 0 || !q.as_bytes()[m.start() - 1].is_ascii_hexdigit())
                .filter(|m| m.end() == q.len() || !q.as_bytes()[m.end()].is_ascii_hexdigit())
                .map(|m| m.as_str().to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            let rows = if ids.is_empty() {
                conn.prepare("SELECT id FROM review_tasks WHERE owner=? AND question=? LIMIT 2")?
                    .query_map(params![owner, q], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                conn.prepare("SELECT id FROM review_tasks WHERE owner=? AND id IN(SELECT value FROM json_each(?)) LIMIT 2")?.query_map(params![owner,encode(&json!(ids))?],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            Ok(if rows.len() == 1 {
                json!(rows[0])
            } else {
                Value::Null
            })
        }
        "recent_reviews" => {
            let rows=conn.prepare("SELECT id,substr(question,1,300),revision FROM review_tasks WHERE owner=? ORDER BY rowid DESC LIMIT 5")?.query_map([owner],|r|Ok(json!({"task_id":r.get::<_,String>(0)?,"goal":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!(rows))
        }
        "history" => {
            task(conn, owner, id)?;
            let offset = number(r, "offset", 0)?;
            if offset < 0 {
                return Err(Failure::invalid("Некорректное смещение истории"));
            }
            let mut rows = events(conn, id, false, offset)?;
            let next = if rows.len() > 8 {
                json!(offset + 8)
            } else {
                Value::Null
            };
            rows.truncate(8);
            Ok(json!({"events":rows,"next_offset":next}))
        }
        "latest_events" => {
            task(conn, owner, id)?;
            Ok(json!(latest(conn, id)?))
        }
        "status" => status(conn, owner, id, &r["fingerprints"]),
        "remember_read" => record::remember(conn, r),
        "record" => record::record(conn, r),
        _ => Err(Failure::invalid("Неизвестное действие проверки документов")),
    }
}
