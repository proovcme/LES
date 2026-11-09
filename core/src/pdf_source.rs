//! Exact, read-only catalog selectors and revision checks for physical PDF reads.
mod preview;
pub use preview::{decode_png, encode_png};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

pub struct Source {
    pub path: PathBuf,
    pub identity: Value,
    pub sha256: String,
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Literal in-page lookup, never semantic inference or an index fallback.
pub fn literal_window(
    text: &str,
    query: &str,
    limit: usize,
) -> Result<(usize, usize), &'static str> {
    if query.chars().count() > 200 {
        return Err("PDF_QUERY_INVALID");
    }
    if query.chars().count() > limit {
        return Err("PDF_TEXT_LIMIT");
    }
    let re = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .map_err(|_| "PDF_QUERY_INVALID")?;
    let found = re.find(text).ok_or("PDF_QUERY_NOT_FOUND")?;
    let position = text[..found.start()].chars().count();
    let size = text.chars().count();
    let start = position.saturating_sub(limit / 3);
    Ok((start, start.saturating_add(limit).min(size)))
}

pub fn resolve(request: &Value) -> Result<Source, &'static str> {
    let database = request["database"]
        .as_str()
        .ok_or("PDF_CATALOG_UNAVAILABLE")?;
    let args = &request["source"];
    let selector = |key| args[key].as_str().unwrap_or("");
    let (id, dataset, name) = (
        selector("doc_id"),
        selector("dataset_id"),
        selector("doc_name"),
    );
    if id.is_empty() && (dataset.is_empty() || name.is_empty()) {
        return Err("PDF_IDENTITY_REQUIRED");
    }
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "PDF_CATALOG_UNAVAILABLE")?;
    let mut statement = conn
        .prepare(
            "SELECT id,dataset_id,file_name,source_path,file_hash FROM documents
        WHERE (?1='' OR id=?1) AND (?2='' OR dataset_id=?2) AND (?3='' OR file_name=?3) LIMIT 2",
        )
        .map_err(|_| "PDF_CATALOG_UNAVAILABLE")?;
    let rows = statement
        .query_map(params![id, dataset, name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })
        .map_err(|_| "PDF_CATALOG_UNAVAILABLE")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "PDF_CATALOG_UNAVAILABLE")?;
    let [(id, dataset, name, path, catalog_hash)] = rows.as_slice() else {
        return Err(if rows.is_empty() {
            "PDF_DOCUMENT_NOT_FOUND"
        } else {
            "PDF_IDENTITY_AMBIGUOUS"
        });
    };
    if !name.to_lowercase().ends_with(".pdf") {
        return Err("PDF_SOURCE_INVALID");
    }
    let path = crate::document_source::resolve(
        path,
        dataset,
        name,
        std::path::Path::new(request["content_root"].as_str().unwrap_or("")),
    )
    .map_err(|code| {
        if code == "SOURCE_PATH_INVALID" {
            "PDF_SOURCE_INVALID"
        } else {
            "PDF_SOURCE_UNAVAILABLE"
        }
    })?;
    let supplied = selector("source_sha256");
    let hash = if supplied.is_empty() {
        catalog_hash.as_str()
    } else {
        supplied
    };
    if !is_sha256(hash) {
        return Err("PDF_REVISION_REQUIRED");
    }
    if is_sha256(catalog_hash) && !hash.eq_ignore_ascii_case(catalog_hash) {
        return Err("PDF_REVISION_CHANGED");
    }
    Ok(Source {
        path,
        sha256: hash.to_lowercase(),
        identity: json!({"doc_id":id,"dataset_id":dataset,"doc_name":name}),
    })
}

pub fn verify(file: &mut File, expected: &str) -> Result<(), &'static str> {
    if fingerprint(file)? != expected {
        return Err("PDF_REVISION_CHANGED");
    }
    Ok(())
}
/// Stable SHA on the same open handle. Also used for originals not indexed yet.
pub fn fingerprint(file: &mut File) -> Result<String, &'static str> {
    let before = file.metadata().map_err(|_| "PDF_SOURCE_UNAVAILABLE")?;
    if !before.is_file() || before.len() > 512 * 1024 * 1024 {
        return Err("PDF_SOURCE_LIMIT");
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "PDF_SOURCE_UNAVAILABLE")?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0;
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "PDF_SOURCE_UNAVAILABLE")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > before.len() {
            return Err("PDF_REVISION_CHANGED");
        }
        hash.update(&buffer[..n]);
    }
    let after = file.metadata().map_err(|_| "PDF_SOURCE_UNAVAILABLE")?;
    if total != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("PDF_REVISION_CHANGED");
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selectors_never_guess_and_revisions_are_checked() {
        let root = std::env::temp_dir().join(format!(
            "les-pdf-source-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let db = root.join("meta.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE documents(id TEXT,dataset_id TEXT,file_name TEXT,source_path TEXT,file_hash TEXT)").unwrap();
        let file = root.join("Документ.pdf");
        std::fs::write(&file, b"original").unwrap();
        let sha = crate::sha256_hex(b"original");
        conn.execute(
            "INSERT INTO documents VALUES('doc','dataset','folder/Документ.pdf',?1,?2)",
            params![file.to_str().unwrap(), sha],
        )
        .unwrap();
        let mut request = json!({"database":db,"source":{"doc_id":"doc","dataset_id":"wrong"}});
        assert_eq!(resolve(&request).err(), Some("PDF_DOCUMENT_NOT_FOUND"));
        request["source"] = json!({"dataset_id":"dataset","doc_name":"Документ.pdf"});
        assert_eq!(resolve(&request).err(), Some("PDF_DOCUMENT_NOT_FOUND"));
        request["source"] = json!({"doc_id":"doc","source_sha256":"0".repeat(64)});
        assert_eq!(resolve(&request).err(), Some("PDF_REVISION_CHANGED"));
        request["source"] = json!({"doc_id":"doc"});
        let source = resolve(&request).unwrap();
        verify(&mut File::open(&source.path).unwrap(), &source.sha256).unwrap();
        std::fs::write(&file, b"modified").unwrap();
        assert_eq!(
            verify(&mut File::open(&file).unwrap(), &source.sha256).err(),
            Some("PDF_REVISION_CHANGED")
        );
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }
}
