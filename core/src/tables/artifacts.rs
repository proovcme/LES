//! Active Parquet references change in the same transaction as document publication.
use super::{field, Result, INVALID, LIMIT};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Component, Path},
};
const CATALOG: &str = "PARQUET_CATALOG_UNAVAILABLE";
fn relative(value: &str) -> bool {
    !value.is_empty()
        && !value.contains([':', '\0'])
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn activate(db: &Connection, request: &Value, name: &str) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS les_table_artifacts (
        dataset_id TEXT NOT NULL,file_name TEXT NOT NULL,path TEXT NOT NULL,
        sha256 TEXT NOT NULL,source_sha256 TEXT NOT NULL,
        PRIMARY KEY(dataset_id,file_name,path))",
    )
    .map_err(|_| CATALOG)?;
    let dataset = field(request, "dataset_id");
    let mut selected = BTreeSet::new();
    for row in request["rows"].as_array().ok_or(INVALID)? {
        let path = field(row, "parquet_path");
        if !path.starts_with("_parquet/.les-versions/") {
            continue;
        }
        let checksum = field(row, "parquet_sha256");
        let source = field(row, "parquet_source_sha256");
        if !relative(path)
            || !hash(checksum)
            || !hash(source)
            || request["fingerprint"]["file_hash"] != source
        {
            return Err(INVALID);
        }
        selected.insert((path, checksum, source));
    }
    db.execute(
        "DELETE FROM les_table_artifacts WHERE dataset_id=?1 AND file_name=?2",
        params![dataset, name],
    )
    .map_err(|_| CATALOG)?;
    for (path, checksum, source) in selected {
        db.execute(
            "INSERT INTO les_table_artifacts VALUES (?1,?2,?3,?4,?5)",
            params![dataset, name, path, checksum, source],
        )
        .map_err(|_| CATALOG)?;
    }
    Ok(())
}
pub fn verify(request: &Value, content_root: &Path) -> Result<()> {
    if !request["rows"]
        .as_array()
        .ok_or(INVALID)?
        .iter()
        .any(|r| field(r, "parquet_path").starts_with("_parquet/.les-versions/"))
    {
        return Ok(());
    }
    let dataset = field(request, "dataset_id");
    if !relative(dataset) || dataset.contains(['/', '\\']) {
        return Err(INVALID);
    }
    let base = content_root
        .join(dataset)
        .canonicalize()
        .map_err(|_| "PARQUET_SOURCE_UNAVAILABLE")?;
    let mut seen = HashSet::new();
    for row in request["rows"].as_array().ok_or(INVALID)? {
        let relative_path = field(row, "parquet_path");
        if !relative_path.starts_with("_parquet/.les-versions/") || !seen.insert(relative_path) {
            continue;
        }
        if !relative(relative_path) {
            return Err(INVALID);
        }
        let path = base
            .join(relative_path)
            .canonicalize()
            .map_err(|_| "PARQUET_SOURCE_UNAVAILABLE")?;
        if !path.starts_with(&base) {
            return Err("PARQUET_PATH_INVALID");
        }
        if super::storage::checksum(&path)? != field(row, "parquet_sha256") {
            return Err("PARQUET_SOURCE_CHANGED");
        }
    }
    Ok(())
}
fn reparse(meta: &fs::Metadata) -> bool {
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
fn walk(
    root: &Path,
    base: &Path,
    excluded: &HashSet<String>,
    out: &mut Vec<Value>,
    dataset: &str,
    count: &mut usize,
) -> Result<()> {
    for entry in fs::read_dir(root).map_err(|_| CATALOG)? {
        let entry = entry.map_err(|_| CATALOG)?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|_| CATALOG)?;
        *count += 1;
        if *count > 100_000 {
            return Err(LIMIT);
        }
        if reparse(&meta) {
            continue;
        }
        if meta.is_dir() {
            if entry.file_name() != ".les-versions" {
                walk(&path, base, excluded, out, dataset, count)?;
            }
            continue;
        }
        if !meta.is_file()
            || path
                .extension()
                .is_none_or(|v| !v.eq_ignore_ascii_case("parquet"))
        {
            continue;
        }
        let rel = path
            .strip_prefix(base)
            .map_err(|_| INVALID)?
            .to_str()
            .ok_or(INVALID)?
            .replace('\\', "/");
        if excluded.contains(&rel) {
            continue;
        }
        let name = Path::new(&rel)
            .strip_prefix("_parquet")
            .map_err(|_| INVALID)?
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        out.push(json!({"dataset_id":dataset,"file_name":name,"parquet_path":rel}));
        if out.len() > 10_000 {
            return Err(LIMIT);
        }
    }
    Ok(())
}
pub(super) fn refs(request: &Value) -> Result<Value> {
    let datasets = request["dataset_ids"]
        .as_array()
        .filter(|v| v.len() <= 512)
        .ok_or(INVALID)?;
    let root = Path::new(field(request, "storage_root"));
    if !root.is_absolute() {
        return Err(INVALID);
    }
    let database = Path::new(field(request, "database"));
    let db = crate::workspace::connection(database, false).map_err(|_| CATALOG)?;
    let has_manifest = if let Some(db) = db.as_ref() {
        db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='les_table_artifacts')",[],|r|r.get::<_,bool>(0)).map_err(|_|CATALOG)?
    } else {
        false
    };
    let mut out = Vec::new();
    let mut count = 0;
    for dataset in datasets {
        let dataset = dataset
            .as_str()
            .filter(|s| relative(s) && !s.contains(['/', '\\']))
            .ok_or(INVALID)?;
        let base = root.join(dataset);
        let mut excluded = HashSet::new();
        if has_manifest {
            let db = db.as_ref().unwrap();
            let mut statement=db.prepare("SELECT a.file_name,a.path,a.sha256,a.source_sha256,d.status,coalesce(d.file_hash,'')
                FROM les_table_artifacts a LEFT JOIN documents d ON d.dataset_id=a.dataset_id AND d.file_name=a.file_name WHERE a.dataset_id=?1 ORDER BY a.file_name,a.path").map_err(|_|CATALOG)?;
            let rows = statement
                .query_map([dataset], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })
                .map_err(|_| CATALOG)?;
            for row in rows {
                let (file, path, checksum, source, status, current) = row.map_err(|_| CATALOG)?;
                excluded.insert(format!(
                    "_parquet/{}",
                    Path::new(&file)
                        .with_extension("parquet")
                        .to_string_lossy()
                        .replace('\\', "/")
                ));
                if status.as_deref() != Some("INDEXED")
                    || current.as_deref() != Some(source.as_str())
                {
                    continue;
                }
                if !relative(&path) || !hash(&checksum) {
                    return Err(INVALID);
                }
                out.push(json!({"dataset_id":dataset,"file_name":file,"parquet_path":path,"parquet_sha256":checksum,"source_sha256":source}));
            }
        }
        let legacy = base.join("_parquet");
        if legacy.is_dir() {
            walk(&legacy, &base, &excluded, &mut out, dataset, &mut count)?;
        }
    }
    out.sort_by_key(|v| {
        format!(
            "{}\0{}\0{}",
            field(v, "dataset_id"),
            field(v, "file_name"),
            field(v, "parquet_path")
        )
    });
    if out.len() > 10_000 {
        return Err(LIMIT);
    }
    Ok(json!({"refs":out,"complete":true,"owner":"rust.tables.artifacts"}))
}
