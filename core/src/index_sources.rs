//! Read the manual batch from SQLite and resolve only its registered sources.
use crate::{
    catalog::library::{register_visibility, visible},
    source_scan::visit_files,
    workspace::connection,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

const INVALID: &str = "INDEX_SOURCE_PLAN_INVALID";
const CATALOG: &str = "INDEX_SOURCE_CATALOG_UNAVAILABLE";
const MISSING: &str = "INDEX_SOURCE_MISSING";
const AMBIGUOUS: &str = "INDEX_SOURCE_AMBIGUOUS";

fn file_key(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|part| match part {
            Component::Normal(name) => name.to_str().filter(|name| *name != "_parquet"),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (!components.is_empty()).then(|| components.join("/"))
}

pub fn plan(request: &Value) -> Result<Value, &'static str> {
    let dataset = request["dataset_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(INVALID)?;
    let absolute = |key: &str| {
        request[key]
            .as_str()
            .map(Path::new)
            .filter(|p| p.is_absolute())
            .ok_or(INVALID)
    };
    let database = absolute("database")?;
    let limit = if request["limit"].is_null() {
        -1
    } else {
        request["limit"].as_i64().ok_or(INVALID)?.max(0)
    };
    let pending = {
        let db = connection(database, false)
            .map_err(|_| CATALOG)?
            .ok_or(CATALOG)?;
        register_visibility(&db).map_err(|_| CATALOG)?;
        if request["count_only"] == true {
            let count: i64 = db
                .query_row(
                    "SELECT count(*) FROM documents
                WHERE dataset_id=?1 AND status='PENDING' AND les_visible_document(file_name)",
                    [dataset],
                    |row| row.get(0),
                )
                .map_err(|_| CATALOG)?;
            return Ok(json!({"remaining_pending":count}));
        }
        let mut query = db.prepare(
            "SELECT file_name, coalesce(source_path,'') FROM documents
             WHERE dataset_id=?1 AND status='PENDING' AND les_visible_document(file_name)
             ORDER BY CASE WHEN complexity='needs_ocr' OR pipeline='markdown_needs_ocr' THEN 1 ELSE 0 END,
                 coalesce(nullif(file_size,0),9223372036854775807), file_name LIMIT ?2"
        ).map_err(|_| CATALOG)?;
        let rows = query
            .query_map(params![dataset, limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| CATALOG)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| CATALOG)?
    };
    let root = absolute("root")?;
    let internal: HashSet<String> = pending
        .iter()
        .filter(|(_, source)| source.is_empty())
        .filter_map(|(name, _)| file_key(Path::new(name)))
        .collect();
    let mut exact = HashMap::new();
    let mut legacy: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut total = 0;
    if !internal.is_empty() {
        visit_files(root, &HashSet::from(["_parquet"]), |path| {
            if path.file_name().is_some_and(|name| name == "_parquet") {
                return Ok(());
            }
            let key = file_key(path.strip_prefix(root).map_err(|_| INVALID)?)
                .ok_or("CORE_SCAN_NAME_INVALID")?;
            if !visible(&key) {
                return Ok(());
            }
            total += 1;
            if internal.contains(&key) {
                exact.insert(key, path);
            } else if let Some((_, basename)) = key.rsplit_once('/') {
                if internal.contains(basename) {
                    legacy
                        .entry(basename.into())
                        .and_modify(|path| *path = None)
                        .or_insert(Some(path));
                }
            }
            Ok(())
        })?;
    }

    let mut selected = Vec::with_capacity(pending.len());
    let mut matched = HashSet::new();
    for (name, source) in pending {
        let (path, key, error) = if !source.is_empty() {
            let path = PathBuf::from(&source);
            let error = (!path.is_absolute()).then_some(INVALID);
            (path, name.clone(), error)
        } else if let Some(key) = file_key(Path::new(&name)) {
            let candidate = match exact.get(&key) {
                Some(path) => Ok(path),
                None => match legacy.get(&key) {
                    Some(Some(path)) => Ok(path),
                    Some(None) => Err(AMBIGUOUS),
                    None => Err(MISSING),
                },
            };
            match candidate {
                Ok(path) => {
                    let key =
                        file_key(path.strip_prefix(root).map_err(|_| INVALID)?).ok_or(INVALID)?;
                    matched.insert(path.clone());
                    (path.clone(), key, None)
                }
                Err(code) => (root.join(&key), key, Some(code)),
            }
        } else {
            (root.to_owned(), name.clone(), Some(INVALID))
        };
        selected.push(
            json!({"path":path,"file_key":key,"db_key":name,"source_path":source,"error":error}),
        );
    }
    Ok(json!({"files":selected,"total_files":total,"matched_internal":matched.len()}))
}
