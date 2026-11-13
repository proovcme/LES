//! Durable folder observations. A failed traversal cannot change file presence.
use super::*;

const SCAN_INTERVAL: f64 = 60.0;
const RETRY_INTERVAL: f64 = 20.0;

pub(super) fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS native_folder_watches(
            id TEXT PRIMARY KEY, dataset_id TEXT NOT NULL, root TEXT NOT NULL,
            canonical_root TEXT, parse_limit INTEGER NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            status TEXT NOT NULL DEFAULT 'QUEUED', reason TEXT NOT NULL DEFAULT '',
            message TEXT NOT NULL DEFAULT '', checked_at TEXT, next_scan REAL NOT NULL DEFAULT 0,
            UNIQUE(dataset_id,root)
         );
         CREATE TABLE IF NOT EXISTS native_folder_watch_jobs(
            job_id TEXT PRIMARY KEY, watch_id TEXT NOT NULL, automatic INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS native_folder_watch_files(
            watch_id TEXT NOT NULL, file_name TEXT NOT NULL, source_path TEXT NOT NULL,
            sha256 TEXT NOT NULL, file_size INTEGER NOT NULL, file_mtime REAL NOT NULL,
            seen_in TEXT NOT NULL, missing INTEGER NOT NULL DEFAULT 0, changed INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(watch_id,file_name)
         );",
    )?;
    Ok(())
}

pub(super) fn bind(
    conn: &Connection,
    job: &str,
    dataset: &str,
    root: &str,
    limit: i64,
    args: &Args,
) -> Result<()> {
    if boolean(args, "watch", false)? {
        conn.execute(
            "INSERT INTO native_folder_watches(id,dataset_id,root,parse_limit)
             VALUES(lower(hex(randomblob(16))),?1,?2,?3)
             ON CONFLICT(dataset_id,root) DO UPDATE SET enabled=1,parse_limit=excluded.parse_limit",
            params![dataset, root, limit],
        )?;
    }
    conn.execute(
        "INSERT INTO native_folder_watch_jobs(job_id,watch_id,automatic)
         SELECT ?1,id,?4 FROM native_folder_watches WHERE dataset_id=?2 AND root=?3 AND enabled=1
         ON CONFLICT(job_id) DO UPDATE SET automatic=min(automatic,excluded.automatic)",
        params![job, dataset, root, boolean(args, "scheduled", false)?],
    )?;
    Ok(())
}

pub(super) fn binding(conn: &Connection, job: &str) -> Result<Value> {
    first(
        conn,
        "SELECT w.id,w.canonical_root,b.automatic FROM native_folder_watch_jobs b
         JOIN native_folder_watches w ON w.id=b.watch_id WHERE b.job_id=?1",
        [job],
    )
}

pub(super) fn due(conn: &Connection) -> Result<Value> {
    if !table(conn, "native_folder_watches")? {
        return Ok(Value::Null);
    }
    let item = first(conn,
        "SELECT w.dataset_id,w.root,w.parse_limit FROM native_folder_watches w
         JOIN datasets d ON d.id=w.dataset_id AND coalesce(d.dataset_scope,'user')<>'system'
         WHERE w.enabled=1 AND w.next_scan<=?1
           AND NOT EXISTS (SELECT 1 FROM native_intakes j WHERE j.dataset_id=w.dataset_id AND j.finished_at IS NULL)
         ORDER BY w.next_scan,w.id LIMIT 1",
        [seconds()],
    )?;
    if item.is_null() {
        return Ok(Value::Null);
    }
    let mut args = item
        .as_object()
        .cloned()
        .ok_or_else(|| Failure::invalid("Invalid watch"))?;
    args.insert("scheduled".into(), json!(true));
    super::enqueue(conn, &args)
}

pub(super) fn startup(conn: &Connection) -> Result<Value> {
    if table(conn, "native_folder_watches")? {
        conn.execute(
            "UPDATE native_folder_watches SET next_scan=0 WHERE enabled=1",
            [],
        )?;
    }
    Ok(Value::Null)
}

pub(super) fn observe(
    conn: &Connection,
    watch: &Value,
    job: &str,
    root: &str,
    files: &[Value],
    now: &str,
) -> Result<()> {
    let id = watch["id"]
        .as_str()
        .ok_or_else(|| Failure::invalid("Invalid watch"))?;
    if watch["canonical_root"]
        .as_str()
        .is_some_and(|old| old != root)
    {
        return Err(Failure::new(
            "INTAKE_SOURCE_CHANGED",
            "Путь наблюдаемой папки изменился.",
        ));
    }
    let mut conflict = conn.prepare(
        "SELECT EXISTS(SELECT 1 FROM documents d JOIN native_folder_watches w ON w.dataset_id=d.dataset_id
         WHERE w.id=?1 AND d.file_name=?2 AND coalesce(d.source_path,'')<>?3)",
    )?;
    let mut upsert = conn.prepare(
        "INSERT INTO native_folder_watch_files(watch_id,file_name,source_path,sha256,file_size,file_mtime,seen_in,changed)
         VALUES(?1,?2,?3,?4,?5,?6,?7,1)
         ON CONFLICT(watch_id,file_name) DO UPDATE SET
           changed=(sha256<>excluded.sha256 OR source_path<>excluded.source_path),
           source_path=excluded.source_path,sha256=excluded.sha256,file_size=excluded.file_size,
           file_mtime=excluded.file_mtime,seen_in=excluded.seen_in,missing=0",
    )?;
    for file in files {
        let name = text(
            file.as_object()
                .ok_or_else(|| Failure::invalid("Invalid watch file"))?,
            "file_name",
        )?;
        let path = file["source_path"]
            .as_str()
            .ok_or_else(|| Failure::invalid("Invalid watch path"))?;
        let hash = file["sha256"]
            .as_str()
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| Failure::invalid("Invalid watch hash"))?;
        if conflict.query_row(params![id, name, path], |row| row.get::<_, bool>(0))? {
            return Err(Failure::new(
                "INTAKE_SOURCE_CONFLICT",
                "В наборе уже есть файл с тем же именем из другой папки.",
            ));
        }
        upsert.execute(params![
            id,
            name,
            path,
            hash,
            file["file_size"].as_i64(),
            file["file_mtime"].as_f64(),
            job
        ])?;
    }
    conn.execute(
        "UPDATE native_folder_watch_files SET missing=1 WHERE watch_id=?1 AND seen_in<>?2 AND missing=0",
        params![id, job],
    )?;
    conn.execute(
        "UPDATE native_folder_watches SET canonical_root=?2,status='AVAILABLE',reason='',message='',
         checked_at=?3,next_scan=?4 WHERE id=?1",
        params![id, root, now, seconds() + SCAN_INTERVAL],
    )?;
    Ok(())
}

pub(super) fn batch(conn: &Connection, job: &str) -> Result<Value> {
    Ok(json!(rows(conn,
        "SELECT f.ordinal,f.source_path,o.file_size,o.file_mtime,o.sha256,o.changed
         FROM native_intake_files f JOIN native_intakes j ON j.id=f.job_id
         JOIN native_folder_watch_jobs b ON b.job_id=j.id JOIN native_folder_watch_files o
           ON o.watch_id=b.watch_id AND o.file_name=f.file_name AND o.seen_in=j.id AND o.source_path=f.source_path
         WHERE j.id=?1 AND f.ordinal>=j.processed ORDER BY f.ordinal LIMIT 25",
        [job],
    )?))
}

pub(super) fn needs_index(conn: &Connection, job: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_intake_files f JOIN native_intakes j ON j.id=f.job_id
         JOIN documents d ON d.dataset_id=j.dataset_id AND d.file_name=f.file_name AND d.source_path=f.source_path
         WHERE f.job_id=?1 AND (d.status='PENDING' OR (d.status='ERROR' AND d.retryable=1 AND d.retry_after<=?2)))",
        params![job, seconds()], |row| row.get(0),
    )?)
}

pub(super) fn identify_batch(conn: &Connection, job: &str, files: &mut [Value]) -> Result<()> {
    let mut query = conn.prepare(
        "SELECT w.canonical_root,o.sha256 FROM native_folder_watch_jobs b
         JOIN native_folder_watches w ON w.id=b.watch_id
         JOIN native_folder_watch_files o ON o.watch_id=w.id AND o.seen_in=b.job_id
         WHERE b.job_id=?1 AND o.file_name=?2 AND o.source_path=?3",
    )?;
    for file in files {
        let (root, hash): (String, String) = query.query_row(
            params![
                job,
                file["file_name"].as_str(),
                file["source_path"].as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        file["watch_root"] = root.into();
        file["watch_sha256"] = hash.into();
    }
    Ok(())
}

pub(super) fn failed(
    conn: &Connection,
    job: &str,
    reason: &str,
    message: &str,
    now: &str,
) -> Result<()> {
    let status = match reason {
        "INTAKE_SOURCE_MISSING"
        | "INTAKE_ACCESS_DENIED"
        | "INTAKE_SOURCE_UNREADABLE"
        | "INTAKE_OUTSIDE_ALLOWED_ROOT"
        | "INTAKE_NOT_DIRECTORY" => "UNAVAILABLE",
        "INTAKE_FILE_CHANGED" | "INTAKE_SOURCE_CHANGED" => "STALE",
        _ => "ERROR",
    };
    conn.execute(
        "UPDATE native_folder_watches SET status=?2,reason=?3,message=?4,checked_at=?5,next_scan=?6
         WHERE id=(SELECT watch_id FROM native_folder_watch_jobs WHERE job_id=?1)",
        params![
            job,
            status,
            reason,
            message,
            now,
            seconds() + RETRY_INTERVAL
        ],
    )?;
    Ok(())
}

pub(super) fn index_deferred(
    conn: &Connection,
    job: &str,
    reason: &str,
    message: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE native_folder_watches SET reason=?2,message=?3,next_scan=?4
         WHERE id=(SELECT watch_id FROM native_folder_watch_jobs WHERE job_id=?1)",
        params![job, reason, message, seconds() + RETRY_INTERVAL],
    )?;
    Ok(())
}

pub(super) fn release(conn: &Connection, job: &str, discard_automatic: bool) -> Result<()> {
    if discard_automatic {
        conn.execute(
            "DELETE FROM native_intakes WHERE id=?1 AND finished_at IS NOT NULL
             AND EXISTS (SELECT 1 FROM native_folder_watch_jobs WHERE job_id=?1 AND automatic=1)",
            [job],
        )?;
    }
    conn.execute(
        "DELETE FROM native_folder_watch_jobs WHERE job_id=?1",
        [job],
    )?;
    Ok(())
}

pub(super) fn list(conn: &Connection, args: &Args) -> Result<Value> {
    if !table(conn, "native_folder_watches")? {
        return Ok(json!({"watches":[],"files":[]}));
    }
    let dataset = args
        .get("dataset_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
    let watches = rows(conn,
        "SELECT w.id,w.dataset_id,d.name AS dataset_name,w.root,w.enabled,
                CASE WHEN w.enabled=0 THEN 'PAUSED' ELSE w.status END AS status,
                w.reason,w.message,w.checked_at,
                (SELECT count(*) FROM native_folder_watch_files f WHERE f.watch_id=w.id AND f.missing=1) AS missing
         FROM native_folder_watches w JOIN datasets d ON d.id=w.dataset_id
         WHERE coalesce(d.dataset_scope,'user')<>'system' AND (?1='' OR w.dataset_id=?1)
           AND (?2='' OR w.id=?2) ORDER BY d.name,w.root LIMIT 200",
        params![dataset,id],
    )?;
    let files = if id.is_empty() || watches.is_empty() {
        Vec::new()
    } else {
        let offset = integer(args, "offset", 0)?.max(0);
        rows(conn,
            "SELECT f.file_name,d.id AS doc_id,
               CASE WHEN f.missing=1 THEN 'MISSING'
                    WHEN d.status='ERROR' THEN 'ERROR'
                    WHEN d.chunk_count>0 AND coalesce(d.file_hash,'')<>f.sha256 THEN 'STALE'
                    WHEN d.status='INDEXED' AND d.chunk_count>0 THEN 'INDEXED'
                    WHEN d.status='SKIPPED' THEN 'SKIPPED' ELSE 'PENDING' END AS status
             FROM native_folder_watch_files f JOIN native_folder_watches w ON w.id=f.watch_id
             LEFT JOIN documents d ON d.dataset_id=w.dataset_id AND d.file_name=f.file_name AND d.source_path=f.source_path
             WHERE f.watch_id=?1 ORDER BY f.missing DESC,f.file_name LIMIT 100 OFFSET ?2",
            params![id,offset],
        )?
    };
    Ok(json!({"watches":watches,"files":files}))
}

pub(super) fn set(conn: &Connection, args: &Args) -> Result<Value> {
    let id = text(args, "id")?;
    let enabled = args
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| Failure::invalid("Укажите, включено ли наблюдение."))?;
    if !table(conn, "native_folder_watches")? || conn.execute(
        "UPDATE native_folder_watches SET enabled=?2,next_scan=0
         WHERE id=?1 AND EXISTS(SELECT 1 FROM datasets WHERE id=dataset_id AND coalesce(dataset_scope,'user')<>'system')",
        params![id,enabled],
    )? != 1 {
        return Err(Failure::missing("Наблюдаемая папка не найдена."));
    }
    Ok(json!({"id":id,"enabled":enabled}))
}
