//! Immutable source rows and revision-bound decisions, independent of dialogue summaries.
mod binding;
mod window;
use crate::workspace::{Failure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};
pub fn contract() -> Value {
    serde_json::from_str(include_str!("../../../config/table-document-tool.json"))
        .expect("checked table contract")
}
pub(crate) fn text<'a>(r: &'a Value, k: &str) -> Result<&'a str> {
    r[k].as_str()
        .ok_or_else(|| Failure::invalid("Некорректные параметры задания"))
}
pub(crate) fn number(r: &Value, k: &str, default: i64) -> Result<i64> {
    r.get(k).map_or(Ok(default), |v| {
        v.as_i64()
            .ok_or_else(|| Failure::invalid("Некорректный бюджет или смещение"))
    })
}
pub(crate) fn encode(v: &Value) -> Result<String> {
    crate::tool_execution::canonical::encode(v).map_err(Failure::invalid)
}
pub(crate) fn digest(v: &Value) -> Result<String> {
    Ok(crate::sha256_hex(encode(v)?.as_bytes()))
}
pub(crate) fn parse(v: &str) -> Result<Value> {
    serde_json::from_str(v)
        .map_err(|_| Failure::invalid("Не удалось прочитать сохранённое задание"))
}
fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS document_tasks(id TEXT PRIMARY KEY,owner TEXT NOT NULL,identity TEXT UNIQUE NOT NULL,question TEXT NOT NULL,source TEXT NOT NULL,scope_key TEXT NOT NULL DEFAULT '',revision INTEGER NOT NULL DEFAULT 0,created TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
    CREATE TABLE IF NOT EXISTS document_rows(task_id TEXT NOT NULL,id TEXT NOT NULL,ordinal INTEGER NOT NULL,source TEXT NOT NULL,decision TEXT,PRIMARY KEY(task_id,id),FOREIGN KEY(task_id) REFERENCES document_tasks(id));
    CREATE TABLE IF NOT EXISTS document_packets(task_id TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,rows TEXT NOT NULL,receipt TEXT,PRIMARY KEY(task_id,id),FOREIGN KEY(task_id) REFERENCES document_tasks(id));
    CREATE TABLE IF NOT EXISTS document_bindings(scope_key TEXT PRIMARY KEY,task_id TEXT NOT NULL REFERENCES document_tasks(id));")?;
    let columns = conn
        .prepare("PRAGMA table_info(document_tasks)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|s| s == "scope_key") {
        conn.execute(
            "ALTER TABLE document_tasks ADD COLUMN scope_key TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    let columns = conn
        .prepare("PRAGMA table_info(document_rows)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, kind) in [("source_sheet", "TEXT"), ("source_row", "INTEGER")] {
        if !columns.iter().any(|s| s == name) {
            conn.execute(
                &format!("ALTER TABLE document_rows ADD COLUMN {name} {kind}"),
                [],
            )?;
        }
    }
    conn.execute("CREATE INDEX IF NOT EXISTS document_rows_unaddressed ON document_rows(task_id) WHERE source_sheet IS NULL OR source_row IS NULL",[])?;
    conn.execute("UPDATE document_rows SET source_sheet=json_extract(source,'$.sheet'),source_row=json_extract(source,'$.row') WHERE source_sheet IS NULL OR source_row IS NULL",[])?;
    conn.execute("CREATE INDEX IF NOT EXISTS document_rows_address ON document_rows(task_id,source_sheet,source_row)",[])?;
    Ok(())
}
fn task(conn: &Connection, owner: &str, id: &str) -> Result<Value> {
    conn.query_row("SELECT owner,question,source,scope_key,revision FROM document_tasks WHERE id=? AND owner=?",params![id,owner],|r|Ok(json!({"owner":r.get::<_,String>(0)?,"question":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?,"scope_key":r.get::<_,String>(3)?,"revision":r.get::<_,i64>(4)?}))).optional()?.ok_or_else(||Failure::invalid("Задание не принадлежит текущему диалогу, файлу и области источников"))
}
fn source(conn: &Connection, owner: &str, id: &str) -> Result<Value> {
    let row = task(conn, owner, id)?;
    let mut source = parse(row["source"].as_str().unwrap())?;
    let rows = conn
        .prepare("SELECT source FROM document_rows WHERE task_id=? ORDER BY ordinal")?
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    source["rows"] = json!(rows.iter().map(|s| parse(s)).collect::<Result<Vec<_>>>()?);
    Ok(source)
}
fn open(
    conn: &Connection,
    owner: &str,
    question: &str,
    document: &Value,
    scope: &str,
) -> Result<Value> {
    let identity = digest(&json!([
        owner,
        question,
        document["sha256"],
        document["schema"]
    ]))?;
    let found = conn
        .query_row(
            "SELECT id FROM document_tasks WHERE identity=?",
            [&identity],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    let id = if let Some(id) = found {
        id
    } else {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| Failure::invalid("Не удалось создать задание"))?;
        let id = random
            .iter()
            .map(|c| format!("{c:02x}"))
            .collect::<String>();
        let mut metadata = document
            .as_object()
            .cloned()
            .ok_or_else(|| Failure::invalid("Некорректный снимок таблицы"))?;
        metadata.remove("rows");
        conn.execute("INSERT INTO document_tasks(id,owner,identity,question,source,scope_key)VALUES(?,?,?,?,?,?)",params![id,owner,identity,question,encode(&json!(metadata))?,scope])?;
        let rows = document["rows"]
            .as_array()
            .filter(|r| r.len() <= 20000)
            .ok_or_else(|| Failure::invalid("Некорректный снимок таблицы"))?;
        let mut stmt =
            conn.prepare("INSERT INTO document_rows(task_id,id,ordinal,source,source_sheet,source_row)VALUES(?,?,?,?,?,?)")?;
        for (i, row) in rows.iter().enumerate() {
            let sheet = text(row, "sheet")?;
            let number = number(row, "row", 0)?;
            if !(1..=20000).contains(&number) {
                return Err(Failure::invalid("Некорректный номер исходной строки"));
            }
            stmt.execute(params![
                id,
                text(row, "id")?,
                i as i64,
                encode(row)?,
                sheet,
                number
            ])?;
        }
        id
    };
    if !scope.is_empty() {
        conn.execute(
            "INSERT OR REPLACE INTO document_bindings VALUES(?,?)",
            params![scope, id],
        )?;
    }
    Ok(json!(id))
}
fn status(conn: &Connection, owner: &str, id: &str) -> Result<Value> {
    let task = task(conn, owner, id)?;
    let mut counts = json!({"pending":0,"reviewed":0,"needs_review":0,"excluded":0});
    let mut total = 0;
    let mut stmt=conn.prepare("SELECT COALESCE(json_extract(decision,'$.status'),'pending'),COUNT(*) FROM document_rows WHERE task_id=? GROUP BY 1")?;
    for row in stmt.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (k, v) = row?;
        if counts.get(&k).is_none() {
            return Err(Failure::invalid("Неизвестное состояние строки"));
        }
        counts[k] = json!(v);
        total += v;
    }
    let mut result = json!({"task_id":id,"revision":task["revision"],"total":total,"coverage_complete":total>0&&counts["pending"]==0,"needs_attention":counts["needs_review"].as_i64().unwrap()>0,"decisions_are_model_authored":true,"source_sha256":parse(task["source"].as_str().unwrap())?["sha256"]});
    result
        .as_object_mut()
        .unwrap()
        .extend(counts.as_object().unwrap().clone());
    Ok(result)
}
fn next(conn: &Connection, r: &Value, owner: &str, id: &str) -> Result<Value> {
    let bytes = number(r, "max_bytes", 6000)?;
    let limit = number(r, "max_rows", 6)?;
    if !(512..=12000).contains(&bytes) || !(1..=12).contains(&limit) {
        return Err(Failure::invalid("Некорректный бюджет пакета"));
    }
    let task = task(conn, owner, id)?;
    let candidates=conn.prepare("SELECT source FROM document_rows WHERE task_id=? AND decision IS NULL ORDER BY ordinal LIMIT ?")?.query_map(params![id,limit],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows = Vec::new();
    for candidate in candidates {
        let row = parse(&candidate)?;
        rows.push(row.clone());
        if encode(&json!(rows))?.len() > bytes as usize {
            rows.pop();
            if rows.is_empty() {
                return Ok(
                    json!({"blocked_row":row["id"],"reason":"Строка превышает бюджет пакета; она не обрезана и не отмечена обработанной","rows":[],"packet_id":null}),
                );
            }
            break;
        }
    }
    let packet = digest(&json!([id, task["revision"], rows]))?;
    conn.execute(
        "INSERT OR IGNORE INTO document_packets(task_id,id,revision,rows)VALUES(?,?,?,?)",
        params![
            id,
            packet,
            task["revision"].as_i64(),
            encode(&json!(rows
                .iter()
                .map(|r| r["id"].clone())
                .collect::<Vec<_>>()))?
        ],
    )?;
    Ok(
        json!({"packet_id":packet,"revision":task["revision"],"rows":rows,"source_sha256":parse(task["source"].as_str().unwrap())?["sha256"]}),
    )
}
fn commit(conn: &Connection, r: &Value, owner: &str, id: &str) -> Result<Value> {
    let decisions = r["decisions"]
        .as_array()
        .filter(|d| !d.is_empty() && d.len() <= 12)
        .ok_or_else(|| Failure::invalid("Нужны решения по всем строкам выданного пакета"))?;
    let task = task(conn, owner, id)?;
    let packet = conn
        .query_row(
            "SELECT revision,rows,receipt FROM document_packets WHERE task_id=? AND id=?",
            params![id, text(r, "packet_id")?],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Failure::invalid("Пакет не выдавался для этого задания"))?;
    let ids = parse(&packet.1)?;
    let ids = ids
        .as_array()
        .ok_or_else(|| Failure::invalid("Некорректный пакет"))?;
    let refs = decisions
        .iter()
        .filter_map(|r| r["row_id"].as_str())
        .collect::<BTreeSet<_>>();
    if decisions.len() != ids.len()
        || refs.len() != decisions.len()
        || ids.iter().any(|s| !refs.contains(s.as_str().unwrap_or("")))
    {
        return Err(Failure::invalid(
            "Нужно ровно одно решение для каждого исходного ID пакета",
        ));
    }
    let mut sorted = decisions.clone();
    sorted.sort_by(|a, b| a["row_id"].as_str().cmp(&b["row_id"].as_str()));
    let receipt = encode(&json!(sorted))?;
    if let Some(saved) = packet.2 {
        if saved != receipt {
            return Err(Failure::invalid(
                "Этот пакет уже сохранён с другим результатом",
            ));
        }
        return Ok(Value::Null);
    }
    if packet.0 != task["revision"].as_i64().unwrap() {
        return Err(Failure::invalid(
            "Пакет устарел; запросите следующий пакет заново",
        ));
    }
    for item in decisions {
        let fields = item
            .as_object()
            .ok_or_else(|| Failure::invalid("Некорректное решение"))?;
        if fields.len() != 4
            || !["row_id", "status", "note", "cells"]
                .iter()
                .all(|k| fields.contains_key(*k))
        {
            return Err(Failure::invalid(
                "Решение содержит неизвестные поля; исходные значения менять нельзя",
            ));
        }
        let state = text(item, "status")?;
        if !matches!(state, "reviewed" | "needs_review" | "excluded") {
            return Err(Failure::invalid("Неизвестный статус строки"));
        }
        if !(1..=1200).contains(&text(item, "note")?.chars().count()) {
            return Err(Failure::invalid(
                "Нужно объяснение решения длиной до 1200 символов",
            ));
        }
        let raw = conn.query_row(
            "SELECT source FROM document_rows WHERE task_id=? AND id=?",
            params![id, text(item, "row_id")?],
            |r| r.get::<_, String>(0),
        )?;
        let source = parse(&raw)?;
        let cells = source["cells"]
            .as_array()
            .ok_or_else(|| Failure::invalid("Некорректная исходная строка"))?;
        if item["cells"].as_array().is_none_or(|refs| {
            refs.is_empty()
                || refs
                    .iter()
                    .any(|s| s.as_str().is_none() || !cells.iter().any(|c| c["coordinate"] == *s))
        }) {
            return Err(Failure::invalid(
                "Укажите точные ячейки текущей строки, подтверждающие наблюдение",
            ));
        }
        if state == "reviewed"
            && cells
                .iter()
                .any(|c| c["kind"] == "error" || c["needs_recalculation"] == true)
        {
            return Err(Failure::invalid(
                "В строке ошибка или формула без результата; требуется needs_review",
            ));
        }
        conn.execute(
            "UPDATE document_rows SET decision=? WHERE task_id=? AND id=?",
            params![encode(item)?, id, text(item, "row_id")?],
        )?;
    }
    conn.execute(
        "UPDATE document_packets SET receipt=? WHERE task_id=? AND id=?",
        params![receipt, id, text(r, "packet_id")?],
    )?;
    conn.execute(
        "UPDATE document_tasks SET revision=revision+1 WHERE id=?",
        [id],
    )?;
    Ok(Value::Null)
}
fn operate(conn: &Connection, action: &str, r: &Value) -> Result<Value> {
    let owner = r["owner"].as_str().unwrap_or("");
    let id = r["task_id"].as_str().unwrap_or("");
    match action{
        "bound_context"=>binding::context(conn,&r["state"]),
        "bound_execute"=>binding::execute(conn,r),
        "bound_receipt"=>{let s=&r["state"];let state=status(conn,text(s,"owner")?,text(s,"task_id")?)?;
            crate::receipts::execute(&json!({"action":"ledger_receipt","kind":"table_document","state":state})).map_err(Failure::invalid)},
        "open_source"=>{
            let document=crate::chat_attachments::table_snapshot(Path::new(text(r,"path")?),text(r,"name")?).map_err(Failure::invalid)?;
            if document["sha256"]!=r["source_sha256"]{return Err(Failure::invalid("Вложение изменилось после проверки"));}
            let owner=digest(&json!([r["scope_key"],document["sha256"],document["schema"]]))?;
            let id=open(conn,&owner,text(r,"question")?,&document,text(r,"scope_key")?)?;
            let mut metadata=document.as_object().unwrap().clone();metadata.remove("rows");
            Ok(json!({"id":id,"owner":owner,"document":metadata}))
        },
        "init"=>Ok(Value::Null),"open"=>open(conn,owner,text(r,"question")?,&r["document"],r["scope_key"].as_str().unwrap_or("")),
        "latest"=>conn.query_row("SELECT t.id,t.owner,t.source FROM document_tasks t JOIN document_bindings b ON b.task_id=t.id WHERE b.scope_key=?",[text(r,"scope_key")?],|r|Ok(json!({"id":r.get::<_,String>(0)?,"owner":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?}))).optional().map(|v|v.unwrap_or(Value::Null)).map_err(Into::into),
        "activate"=>{let task=task(conn,owner,id)?;if let Some(scope)=task["scope_key"].as_str().filter(|s|!s.is_empty()){conn.execute("INSERT OR REPLACE INTO document_bindings VALUES(?,?)",params![scope,id])?;}Ok(Value::Null)},
        "new_question"=>{let task=task(conn,owner,id)?;open(conn,owner,text(r,"question")?,&source(conn,owner,id)?,task["scope_key"].as_str().unwrap())},
        "snapshot"=>source(conn,owner,id),"status"=>status(conn,owner,id),"matches_question"=>Ok(json!(task(conn,owner,id)?["question"]==r["question"])),
        "recent"=>{let rows=conn.prepare("SELECT id,question,revision FROM document_tasks WHERE owner=? ORDER BY rowid DESC LIMIT 5")?.query_map([owner],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(json!(rows.into_iter().map(|(id,q,v)|json!({"task_id":id,"question_preview":q.chars().take(300).collect::<String>(),"question_truncated":q.chars().count()>300,"revision":v})).collect::<Vec<_>>()))},
        "next_packet"=>next(conn,r,owner,id),"commit"=>commit(conn,r,owner,id),
        "results"=>{task(conn,owner,id)?;let offset=number(r,"offset",0)?;let bytes=number(r,"max_bytes",9000)?;if offset<0||bytes<0{return Err(Failure::invalid("Некорректное смещение"));}
            let mut output=Vec::new();let rows=conn.prepare("SELECT id,source,decision FROM document_rows WHERE task_id=? ORDER BY ordinal LIMIT 12 OFFSET ?")?.query_map(params![id,offset],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            for(key,s,d)in rows{let item=json!({"source":parse(&s)?,"decision":d.as_deref().map(parse).transpose()?.unwrap_or(Value::Null)});output.push(item);if encode(&json!(output))?.len()>bytes as usize{output.pop();if output.is_empty(){return Ok(json!({"rows":[],"next_offset":offset,"blocked_row":key}));}break;}}
            let total:i64=conn.query_row("SELECT count(*) FROM document_rows WHERE task_id=?",[id],|r|r.get(0))?;let next=offset+output.len()as i64;Ok(json!({"rows":output,"next_offset":if next<total{json!(next)}else{Value::Null}}))},
        "read_window"|"read_cell"=>addressed(conn,owner,id,action,r),
        _=>Err(Failure::invalid("Неизвестное действие задания"))
    }
}
fn addressed(
    conn: &Connection,
    owner: &str,
    id: &str,
    action: &str,
    args: &Value,
) -> Result<Value> {
    let task = task(conn, owner, id)?;
    let mut doc = parse(task["source"].as_str().unwrap())?;
    let sheet = text(args, "sheet")?;
    let height: i64 = conn.query_row(
        "SELECT COALESCE(MAX(source_row),0) FROM document_rows WHERE task_id=? AND source_sheet=?",
        params![id, sheet],
        |r| r.get(0),
    )?;
    let start = if action == "read_cell" {
        window::address(text(args, "coordinate")?)
            .ok_or_else(|| Failure::invalid("Некорректная ячейка"))?
            .0
    } else {
        number(args, "row_start", 1)?
    };
    let stop = if action == "read_cell" {
        start
    } else {
        start.saturating_add(number(args, "row_count", 4)?.clamp(1, 4) - 1)
    };
    let rows=conn.prepare("SELECT source FROM document_rows WHERE task_id=? AND source_sheet=? AND source_row BETWEEN ? AND ? ORDER BY source_row")?
        .query_map(params![id,sheet,start,stop],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    doc["rows"] = json!(rows.iter().map(|r| parse(r)).collect::<Result<Vec<_>>>()?);
    doc["source_height"] = json!(height);
    window::execute(&doc, action, args)
}
pub fn execute(req: &Value) -> Value {
    match transport(req) {
        Ok(v) => json!({"status":"completed","value":v}),
        Err(e) => json!({"status":"failed","code":e.code,"detail":e.detail}),
    }
}
fn transport(req: &Value) -> Result<Value> {
    let mut input = req.clone();
    if let Some(name) = req["input_file"].as_str() {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(name)
            .map_err(|_| Failure::invalid("Не удалось прочитать запрос задания"))?
            .take(512 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::invalid("Не удалось прочитать запрос задания"))?;
        if bytes.len() > 512 * 1024 * 1024 {
            return Err(Failure::invalid(
                "Снимок таблицы превышает лимит обмена 512 МиБ",
            ));
        }
        input = serde_json::from_slice(&bytes)
            .map_err(|_| Failure::invalid("Некорректный запрос задания"))?;
    }
    let value = run(&input)?;
    if let Some(name) = req["output_file"].as_str() {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(name)
            .map_err(|_| Failure::invalid("Не удалось сохранить ответ задания"))?;
        serde_json::to_writer(&mut file, &value)
            .map_err(|_| Failure::invalid("Не удалось сохранить ответ задания"))?;
        file.flush()
            .map_err(|_| Failure::invalid("Не удалось сохранить ответ задания"))?;
        return Ok(Value::Null);
    }
    Ok(value)
}
fn run(req: &Value) -> Result<Value> {
    if req["schema"] != "les.document-tasks.v1" {
        return Err(Failure::invalid("Некорректная схема задания"));
    }
    if let Some(action) = req["action"]
        .as_str()
        .and_then(|s| s.strip_prefix("review_"))
    {
        return crate::document_review::run(req, action);
    }
    if req["action"] == "contract" {
        return Ok(contract());
    }
    if req["action"] == "window" {
        return window::execute(
            &req["arguments"]["document"],
            text(&req["arguments"], "operation")?,
            &req["arguments"],
        );
    }
    let path = Path::new(text(req, "database")?);
    if !path.is_absolute() {
        return Err(Failure::invalid("Требуется абсолютный путь к базе заданий"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| Failure::invalid("Не удалось создать каталог заданий"))?;
    }
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(15))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    initialize(&tx)?;
    let value = operate(&tx, text(req, "action")?, &req["arguments"])?;
    tx.commit()?;
    Ok(value)
}
