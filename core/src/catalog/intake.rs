//! SQLite checkpoints for the external folder intake worker.
use super::*;
use rusqlite::params;
mod watch;

const FIELDS: &str = "j.id,'dataset_intake' AS type,'native_external' AS source,j.dataset_id,
    j.dataset_name,j.status,j.phase,j.total,j.processed,j.errors,j.message,j.reason_code,
    j.index_pending,j.indexed,j.index_errors,j.index_skipped,j.recoveries,j.started_at,j.updated_at,j.finished_at";

pub(super) fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS native_intakes (
            id TEXT PRIMARY KEY, dataset_id TEXT NOT NULL, dataset_name TEXT NOT NULL,
            root TEXT NOT NULL, resolved_root TEXT, parse_limit INTEGER NOT NULL,
            status TEXT NOT NULL, phase TEXT NOT NULL, total INTEGER NOT NULL DEFAULT 0,
            processed INTEGER NOT NULL DEFAULT 0, errors INTEGER NOT NULL DEFAULT 0,
            message TEXT NOT NULL, reason_code TEXT NOT NULL DEFAULT '',
            recoveries INTEGER NOT NULL DEFAULT 0, indexed INTEGER NOT NULL DEFAULT 0,
            index_pending INTEGER NOT NULL DEFAULT 0, index_errors INTEGER NOT NULL DEFAULT 0,
            index_skipped INTEGER NOT NULL DEFAULT 0,
            retry_at REAL NOT NULL DEFAULT 0, started_at TEXT NOT NULL,
            updated_at TEXT NOT NULL, finished_at TEXT
         );
         DROP INDEX IF EXISTS native_intakes_active_dataset;
         DROP INDEX IF EXISTS native_intakes_active_folder_dataset;
         CREATE UNIQUE INDEX IF NOT EXISTS native_intakes_active_directory_dataset
         ON native_intakes(dataset_id) WHERE finished_at IS NULL AND root NOT IN ('les:chat-index:v1','les:mail-index:v1');
         CREATE TABLE IF NOT EXISTS les_indexed_attachments(
            id TEXT PRIMARY KEY, owner TEXT NOT NULL, key_hash TEXT NOT NULL,
            fingerprint TEXT NOT NULL, dataset_id TEXT NOT NULL, doc_id TEXT NOT NULL,
            job_id TEXT NOT NULL UNIQUE, sha256 TEXT NOT NULL, response TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS native_intake_files (
            job_id TEXT NOT NULL, ordinal INTEGER NOT NULL, file_name TEXT NOT NULL,
            source_path TEXT NOT NULL, PRIMARY KEY(job_id,ordinal)
         );",
    )?;
    watch::init(conn)
}

fn public_job(conn: &Connection, id: &str) -> Result<Value> {
    let mut job = first(
        conn,
        &format!(
            "SELECT {FIELDS} FROM native_intakes j JOIN datasets d ON d.id=j.dataset_id
                  WHERE j.id=?1 AND COALESCE(d.dataset_scope,'user')<>'system'"
        ),
        [id],
    )?;
    with_current_stage(conn, &mut job)?;
    Ok(job)
}

fn with_current_stage(conn: &Connection, job: &mut Value) -> Result<()> {
    if job["phase"] != "index" || job["status"] != "running" {
        return Ok(());
    }
    let current = first(
        conn,
        "SELECT file_name,stage FROM documents WHERE dataset_id=?1 AND status='PENDING'
         AND stage IN ('WAITING_RESOURCES','CONVERT','EMBED','UPSERT')
         ORDER BY CASE stage WHEN 'UPSERT' THEN 0 WHEN 'EMBED' THEN 1 ELSE 2 END,file_name LIMIT 1",
        [job["dataset_id"].as_str()],
    )?;
    let Some(stage) = current["stage"].as_str() else {
        return Ok(());
    };
    let label = match stage {
        "WAITING_RESOURCES" => "Ожидаю свободную память",
        "CONVERT" => "Читаю документ",
        "EMBED" => "Готовлю данные для поиска",
        "UPSERT" => "Сохраняю индекс",
        _ => return Ok(()),
    };
    let file = current["file_name"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    job["current_stage"] = json!(stage);
    job["current_file"] = json!(file);
    job["message"] = json!(format!("{label} · {file}"));
    Ok(())
}

fn enqueue(conn: &Connection, args: &Args) -> Result<Value> {
    init(conn)?;
    let dataset = text(args, "dataset_id")?;
    let name: Option<String> = conn.query_row(
        "SELECT coalesce(name,id) FROM datasets WHERE id=?1 AND coalesce(dataset_scope,'user')<>'system'",
        [dataset], |row| row.get(0),
    ).optional()?;
    let name = name.ok_or_else(|| Failure::missing("Набор документов не найден."))?;
    let root = text(args, "root")?;
    if matches!(
        root,
        super::INDEXED_ATTACHMENT_ROOT | super::mail::INDEX_ROOT
    ) {
        return Err(Failure::invalid("Укажите путь к папке."));
    }
    let parse_limit = integer(args, "parse_limit", 0)?;
    let existing = first(
        conn,
        "SELECT id,root,parse_limit FROM native_intakes
        WHERE dataset_id=?1 AND finished_at IS NULL",
        [dataset],
    )?;
    if let Some(id) = existing["id"].as_str() {
        if existing["root"] != root || existing["parse_limit"] != parse_limit {
            return Err(Failure::conflict(
                "Для этого набора уже подключается папка. Дождитесь завершения.",
            ));
        }
        watch::bind(conn, id, dataset, root, parse_limit, args)?;
        return public_job(conn, id);
    }
    let id: String = conn.query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
    let now = timestamp();
    conn.execute(
        "INSERT INTO native_intakes(id,dataset_id,dataset_name,root,parse_limit,status,phase,message,started_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,'queued','scan','Папка в очереди на проверку',?6,?6)",
        params![id,dataset,name,root,parse_limit,now],
    )?;
    watch::bind(conn, &id, dataset, root, parse_limit, args)?;
    public_job(conn, &id)
}

fn claim(conn: &Connection, now: &str) -> Result<Value> {
    init(conn)?;
    // Only the process holding the worker lease calls claim. A running row
    // here belongs to a previous invocation that stopped before finishing.
    let job = first(
        conn,
        "SELECT id,status FROM native_intakes WHERE finished_at IS NULL AND retry_at<=?1
         ORDER BY CASE status WHEN 'running' THEN 0 ELSE 1 END,
         CASE phase WHEN 'index' THEN 1 ELSE 0 END,updated_at,id LIMIT 1",
        [seconds()],
    )?;
    let Some(id) = job["id"].as_str() else {
        return Ok(Value::Null);
    };
    conn.execute(
        "UPDATE native_intakes SET recoveries=recoveries+(status='running'),
         reason_code=CASE status WHEN 'running' THEN 'INTAKE_INTERRUPTED' ELSE reason_code END,
         message=CASE WHEN status='running' THEN 'Импорт прервался. Продолжаю с сохранённого этапа'
                      WHEN phase='scan' THEN 'Проверяю состав папки' ELSE message END,
         status='running',updated_at=?2 WHERE id=?1",
        params![id, now],
    )?;
    let mut claimed = first(conn, "SELECT * FROM native_intakes WHERE id=?1", [id])?;
    if claimed["root"] == super::INDEXED_ATTACHMENT_ROOT {
        claimed["attachment_sha256"] = first(
            conn,
            "SELECT sha256 FROM les_indexed_attachments WHERE job_id=?1",
            [id],
        )?["sha256"]
            .clone();
    }
    if claimed["root"] == super::mail::INDEX_ROOT {
        claimed["attachment_sha256"] = first(
            conn,
            "SELECT sha256 FROM les_mail_index_jobs WHERE job_id=?1",
            [id],
        )?["sha256"]
            .clone();
    }
    claimed["resumed"] = json!(job["status"] == "running");
    claimed["watch"] = watch::binding(conn, id)?;
    Ok(claimed)
}

fn manifest(conn: &Connection, id: &str, args: &Args, now: &str) -> Result<Value> {
    let files = args["files"]
        .as_array()
        .ok_or_else(|| Failure::invalid("Invalid intake manifest"))?;
    let watched = watch::binding(conn, id)?;
    if (files.is_empty() && watched.is_null()) || files.len() > 50_000 {
        return Err(Failure::invalid("Invalid intake manifest length"));
    }
    let updated = conn.execute(
        "UPDATE native_intakes SET phase='register',resolved_root=?2,total=?3,updated_at=?4,
         message='Регистрирую документы' WHERE id=?1 AND phase='scan' AND status='running'",
        params![id, text(args, "root")?, files.len() as i64, now],
    )?;
    if updated != 1 {
        return Err(Failure::conflict("Состояние импорта изменилось."));
    }
    if !watched.is_null() {
        watch::observe(conn, &watched, id, text(args, "root")?, files, now)?;
    }
    let mut insert = conn.prepare(
        "INSERT INTO native_intake_files(job_id,ordinal,file_name,source_path) VALUES(?1,?2,?3,?4)",
    )?;
    for (ordinal, file) in files.iter().enumerate() {
        insert.execute(params![
            id,
            ordinal as i64,
            file["file_name"].as_str(),
            file["source_path"].as_str()
        ])?;
    }
    Ok(Value::Null)
}

fn register(conn: &Connection, id: &str, args: &Args, now: &str) -> Result<Value> {
    let job = first(conn, "SELECT j.dataset_id,j.total,j.processed,j.parse_limit
        FROM native_intakes j JOIN datasets d ON d.id=j.dataset_id
        WHERE j.id=?1 AND j.phase='register' AND j.status='running' AND coalesce(d.dataset_scope,'user')<>'system'", [id])?;
    let dataset = job["dataset_id"]
        .as_str()
        .ok_or_else(|| Failure::missing("Набор документов удалён."))?;
    let processed = job["processed"].as_i64().unwrap_or_default();
    let total = job["total"].as_i64().unwrap_or_default();
    let watched = !watch::binding(conn, id)?.is_null();
    let files = args["files"]
        .as_array()
        .filter(|files| files.len() <= 25 && (!files.is_empty() || watched && total == 0))
        .ok_or_else(|| Failure::invalid("Invalid intake batch"))?;
    for (offset, file) in files.iter().enumerate() {
        let ordinal = processed + offset as i64;
        let item = first(
            conn,
            "SELECT file_name,source_path FROM native_intake_files WHERE job_id=?1 AND ordinal=?2",
            params![id, ordinal],
        )?;
        let name = item["file_name"]
            .as_str()
            .ok_or_else(|| Failure::conflict("Список файлов изменился."))?;
        let path = item["source_path"].as_str().unwrap_or_default();
        if file["ordinal"] != ordinal || file["source_path"] != path {
            return Err(Failure::conflict("Список файлов изменился."));
        }
        let size = file["file_size"]
            .as_i64()
            .ok_or_else(|| Failure::invalid("Invalid source size"))?;
        let mtime = file["file_mtime"]
            .as_f64()
            .ok_or_else(|| Failure::invalid("Invalid source time"))?;
        let existing = first(
            conn,
            "SELECT id,COALESCE(source_path,'') AS source_path,status,file_hash,
            COALESCE(file_size,0) AS file_size,COALESCE(file_mtime,0) AS file_mtime
            FROM documents WHERE dataset_id=?1 AND file_name=?2 ORDER BY rowid LIMIT 1",
            params![dataset, name],
        )?;
        if let Some(doc_id) = existing["id"].as_str() {
            let compared = file["sha256"]
                .as_str()
                .filter(|_| existing["status"] == "INDEXED");
            let same_content = compared.is_some_and(|hash| existing["file_hash"] == hash);
            let same_path = existing["source_path"] == path;
            let same_size = existing["file_size"].as_i64() == Some(size);
            let same_time = existing["file_mtime"].as_f64() == Some(mtime);
            if same_content && same_path && same_size {
                if !same_time {
                    conn.execute(
                        "UPDATE documents SET file_mtime=?2 WHERE id=?1",
                        params![doc_id, mtime],
                    )?;
                }
            } else if (watched && file["changed"] == 1)
                || compared.is_some()
                || !same_size
                || !same_time
                || existing["status"]
                    .as_str()
                    .is_some_and(|status| status.eq_ignore_ascii_case("missing"))
                || !same_path
            {
                conn.execute("UPDATE documents SET status='PENDING',file_size=?2,file_mtime=?3,source_path=?4 WHERE id=?1",
                             params![doc_id,size,mtime,path])?;
                if watched && file["changed"] == 1 {
                    conn.execute("UPDATE documents SET parse_attempts=0,retryable=0,retry_after=0,last_error='',error_code='',stage='' WHERE id=?1", [doc_id])?;
                }
            }
        } else {
            conn.execute(
                "INSERT INTO documents(id,dataset_id,file_name,status,file_size,file_mtime,source_path)
                 VALUES(lower(hex(randomblob(4))||'-'||hex(randomblob(2))||'-4'||substr(hex(randomblob(2)),2)||
                    '-8'||substr(hex(randomblob(2)),2)||'-'||hex(randomblob(6))),?1,?2,'PENDING',?3,?4,?5)",
                params![dataset,name,size,mtime,path],
            )?;
        }
    }
    let processed = processed + files.len() as i64;
    let complete = processed == total;
    let parse = job["parse_limit"].as_i64().unwrap_or_default() > 0
        && (!watched || !complete || watch::needs_index(conn, id)?);
    let (status, phase, message) = match (complete, parse) {
        (true, true) => (
            "running",
            "index",
            "Файлы зарегистрированы. Запускаю индексацию".into(),
        ),
        (true, false) => ("completed", "registered", "Файлы зарегистрированы".into()),
        (false, _) => (
            "running",
            "register",
            format!("Зарегистрировано {processed} из {total}"),
        ),
    };
    conn.execute(
        "UPDATE native_intakes SET processed=?2,status=?3,phase=?4,updated_at=?5,message=?6,
         index_pending=CASE WHEN ?4='index' THEN total ELSE 0 END,
         finished_at=CASE WHEN ?3='completed' THEN ?5 END WHERE id=?1",
        params![id, processed, status, phase, now, message],
    )?;
    if complete && !parse {
        conn.execute("DELETE FROM native_intake_files WHERE job_id=?1", [id])?;
        if watched {
            watch::release(conn, id, true)?;
        }
    }
    Ok(json!({"processed":processed,"total":total,"phase":phase}))
}

fn checkpoint(conn: &Connection, id: &str, args: &Args, now: &str) -> Result<Value> {
    let reason = text(args, "reason")?;
    let note = text(args, "message")?;
    let counts = first(conn, "SELECT
         sum(CASE WHEN upper(d.status)='INDEXED' AND d.chunk_count>0 THEN 1 ELSE 0 END) AS indexed,
         sum(CASE WHEN d.id IS NULL OR upper(coalesce(d.status,'')) NOT IN ('PENDING','PARSING','RUNNING','QUEUED','SKIPPED')
                       AND (upper(coalesce(d.status,''))<>'INDEXED' OR coalesce(d.chunk_count,0)<=0)
                       AND NOT (upper(coalesce(d.status,''))='ERROR' AND coalesce(d.retryable,0)=1)
                  THEN 1 ELSE 0 END) AS errors,
         sum(CASE WHEN upper(d.status) IN ('PENDING','PARSING','RUNNING','QUEUED')
                    OR upper(d.status)='ERROR' AND coalesce(d.retryable,0)=1 THEN 1 ELSE 0 END) AS pending,
         sum(CASE WHEN upper(d.status)='SKIPPED' THEN 1 ELSE 0 END) AS skipped
         FROM native_intake_files f JOIN native_intakes j ON j.id=f.job_id
         LEFT JOIN documents d ON d.id=(SELECT candidate.id FROM documents candidate
             WHERE candidate.dataset_id=j.dataset_id AND candidate.file_name=f.file_name
             ORDER BY candidate.rowid LIMIT 1)
             AND d.source_path=f.source_path
         WHERE j.id=?1 AND j.phase='index'", [id])?;
    let indexed = counts["indexed"].as_i64().unwrap_or_default();
    let errors = counts["errors"].as_i64().unwrap_or_default();
    let pending = counts["pending"].as_i64().unwrap_or_default();
    let skipped = counts["skipped"].as_i64().unwrap_or_default();
    let watched = !watch::binding(conn, id)?.is_null();
    let deferred = watched && pending > 0 && !reason.is_empty();
    let (status, reason, message, retry_at) = if deferred {
        watch::index_deferred(conn, id, reason, note)?;
        ("failed", reason, note.to_owned(), 0.0)
    } else if pending == 0 && errors > 0 {
        ("failed", "INDEX_DOCUMENT_ERRORS", format!("Проиндексировано {indexed}, с ошибками: {errors}. Откройте список документов для подробностей."), 0.0)
    } else if pending == 0 && skipped > 0 {
        ("completed", "", format!("Проиндексировано {indexed}, пропущено {skipped}. Причины указаны в списке документов."), 0.0)
    } else if pending == 0 {
        (
            "completed",
            "",
            format!("Документы готовы к поиску. Проиндексировано: {indexed}."),
            0.0,
        )
    } else if reason.is_empty() {
        (
            "queued",
            "",
            format!("Проиндексировано {indexed}, осталось {pending}, с ошибками: {errors}"),
            0.0,
        )
    } else {
        ("waiting", reason, note.to_owned(), seconds() + 30.0)
    };
    conn.execute(
        "UPDATE native_intakes SET indexed=?2,index_errors=?3,index_pending=?4,status=?5,errors=?3,
         reason_code=CASE WHEN ?6='' AND recoveries>0 THEN 'INTAKE_INTERRUPTED' ELSE ?6 END,
         message=?7,retry_at=?8,updated_at=?9,
         finished_at=CASE WHEN ?5 IN ('completed','failed') THEN ?9 END,index_skipped=?10
         WHERE id=?1 AND phase='index' AND status='running'",
        params![id, indexed, errors, pending, status, reason, message, retry_at, now, skipped],
    )?;
    if status == "completed" && indexed > 0 {
        let dataset: String = conn.query_row(
            "SELECT dataset_id FROM native_intakes WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        crate::index_events::completed(conn, &format!("intake:{id}"), &dataset, seconds())?;
    }
    if matches!(status, "completed" | "failed") {
        conn.execute("DELETE FROM native_intake_files WHERE job_id=?1", [id])?;
        if watched {
            watch::release(conn, id, deferred)?;
        }
    }
    Ok(
        json!({"status":status,"indexed":indexed,"errors":errors,"pending":pending,"skipped":skipped}),
    )
}

pub(super) fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    match action {
        "intake_watch_startup" => return watch::startup(conn),
        "intake_watch_due" => return watch::due(conn),
        "intake_watch_list" => return watch::list(conn, args),
        "intake_watch_set" => return watch::set(conn, args),
        _ => {}
    }
    if action == "intake_enqueue" {
        return enqueue(conn, args);
    }
    if action == "intake_list" {
        if !table(conn, "native_intakes")? {
            return Ok(json!({}));
        }
        let limit = integer(args, "limit", 200)?.clamp(1, 500);
        let mut rows = rows(
            conn,
            &format!(
                "SELECT {FIELDS} FROM native_intakes j JOIN datasets d
          ON d.id=j.dataset_id WHERE coalesce(d.dataset_scope,'user')<>'system'
          ORDER BY j.updated_at DESC LIMIT ?1"
            ),
            [limit],
        )?;
        for row in &mut rows {
            with_current_stage(conn, row)?;
        }
        return Ok(Value::Object(
            rows.into_iter()
                .filter_map(|row| {
                    let id = row["id"].as_str()?.to_owned();
                    Some((id, row))
                })
                .collect(),
        ));
    }
    if action == "intake_owned_datasets" {
        if !table(conn, "native_intakes")? {
            return Ok(json!([]));
        }
        let owned = rows(
            conn,
            "SELECT dataset_id FROM native_intakes WHERE finished_at IS NULL",
            [],
        )?;
        return Ok(Value::Array(
            owned
                .into_iter()
                .map(|row| row["dataset_id"].clone())
                .collect(),
        ));
    }
    let now = timestamp();
    if action == "intake_claim" {
        return claim(conn, &now);
    }
    if action == "intake_has_work" {
        let ready = table(conn, "native_intakes")? && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_intakes WHERE finished_at IS NULL AND retry_at<=?1)",
            [seconds()], |row| row.get::<_, bool>(0),
        )?;
        return Ok(json!(ready));
    }
    if action == "intake_active_index" {
        if !table(conn, "native_intakes")? {
            return Ok(json!([]));
        }
        let active = rows(
            conn,
            "SELECT id FROM native_intakes WHERE status='running' AND phase='index'",
            [],
        )?;
        return Ok(Value::Array(
            active.into_iter().map(|row| row["id"].clone()).collect(),
        ));
    }
    let id = text(args, "id")?;
    match action {
        "intake_get" if !table(conn, "native_intakes")? => Ok(Value::Null),
        "intake_get" => public_job(conn, id),
        "intake_manifest" => manifest(conn, id, args, &now),
        "intake_batch" if !watch::binding(conn, id)?.is_null() => watch::batch(conn, id),
        "intake_batch" => Ok(json!(rows(
            conn,
            "SELECT f.ordinal,f.source_path FROM native_intake_files f
             JOIN native_intakes j ON j.id=f.job_id WHERE j.id=?1 AND f.ordinal>=j.processed
             ORDER BY f.ordinal LIMIT 25",
            [id]
        )?)),
        "intake_index_batch" => {
            let mut selected = rows(
            conn,
            "SELECT d.file_name,d.source_path FROM native_intakes j
             JOIN native_intake_files f ON f.job_id=j.id
             JOIN documents d ON d.id=(SELECT candidate.id FROM documents candidate
                 WHERE candidate.dataset_id=j.dataset_id AND candidate.file_name=f.file_name
                 ORDER BY candidate.rowid LIMIT 1) AND d.source_path=f.source_path
             WHERE j.id=?1 AND j.phase='index' AND j.status='running' AND d.status='PENDING'
             ORDER BY CASE WHEN d.complexity='needs_ocr' OR d.pipeline='markdown_needs_ocr' THEN 1 ELSE 0 END,
                      coalesce(nullif(d.file_size,0),9223372036854775807),d.file_name
             LIMIT (SELECT max(0, min(parse_limit, 500)) FROM native_intakes WHERE id=?1)",
            [id]
            )?;
            if !watch::binding(conn, id)?.is_null() {
                watch::identify_batch(conn, id, &mut selected)?;
            }
            Ok(json!(selected))
        }
        "intake_register" => register(conn, id, args, &now),
        "intake_retry_due" => {
            let changed = conn.execute(
                "UPDATE documents SET status='PENDING',stage='' WHERE status='ERROR' AND retryable=1 AND retry_after<=?2
                 AND EXISTS(SELECT 1 FROM native_intakes j JOIN native_intake_files f ON f.job_id=j.id
                    WHERE j.id=?1 AND j.status='running' AND j.phase='index'
                    AND documents.dataset_id=j.dataset_id AND documents.file_name=f.file_name
                    AND documents.source_path=f.source_path)", params![id,seconds()])?;
            Ok(json!({"retried":changed}))
        }
        "intake_index_attempt" => {
            let file = text(args, "file_name")?;
            let source = text(args, "source_path")?;
            let attempts: Option<i64> = conn.query_row(
                "UPDATE documents SET parse_attempts=coalesce(parse_attempts,0)+1,
                    last_attempt_at=?4,retryable=0,retry_after=0,error_code='',last_error='',stage='CONVERT'
                 WHERE status='PENDING' AND id=(SELECT d.id FROM native_intakes j
                    JOIN native_intake_files f ON f.job_id=j.id
                    JOIN documents d ON d.dataset_id=j.dataset_id AND d.file_name=f.file_name
                    WHERE j.id=?1 AND j.phase='index' AND j.status='running'
                      AND f.file_name=?2 AND f.source_path=?3 AND d.source_path=f.source_path
                    ORDER BY d.rowid LIMIT 1)
                 RETURNING parse_attempts",
                params![id, file, source, seconds()], |row| row.get(0),
            ).optional()?;
            Ok(json!(attempts.unwrap_or(0)))
        }
        "intake_index_checkpoint" => checkpoint(conn, id, args, &now),
        "intake_wait" => {
            conn.execute(
                "UPDATE native_intakes SET status='waiting',reason_code=?2,message=?3,retry_at=?4,
                 updated_at=?5 WHERE id=?1 AND finished_at IS NULL",
                params![
                    id,
                    text(args, "reason")?,
                    text(args, "message")?,
                    seconds() + 3.0,
                    now
                ],
            )?;
            Ok(Value::Null)
        }
        "intake_fail" => {
            let reason = text(args, "reason")?;
            let message = text(args, "message")?;
            let watched = !watch::binding(conn, id)?.is_null();
            conn.execute(
                "UPDATE native_intakes SET status='failed',errors=1,message=?2,updated_at=?3,finished_at=?3,
                 reason_code=?4
                 WHERE id=?1 AND finished_at IS NULL",
                params![id,message,now,reason],
            )?;
            conn.execute("DELETE FROM native_intake_files WHERE job_id=?1", [id])?;
            if watched {
                watch::failed(conn, id, reason, message, &now)?;
                watch::release(conn, id, true)?;
            }
            Ok(Value::Null)
        }
        _ => Err(Failure::invalid("Unknown intake action")),
    }
}
