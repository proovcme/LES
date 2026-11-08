//! Immutable prepared-node input and one-file spool ownership.
use super::{Result, INVALID};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
pub(super) struct Spools {
    pub(super) nodes: PathBuf,
    pub(super) prepared: PathBuf,
    pub(super) points: PathBuf,
    pub(super) cache: PathBuf,
}
impl Drop for Spools {
    fn drop(&mut self) {
        for path in [&self.nodes, &self.prepared, &self.points, &self.cache] {
            let _ = std::fs::remove_file(path);
        }
    }
}
pub(super) fn prepare(
    input: &Path,
    output: &Path,
    count: u64,
    config: &Value,
    dataset: &str,
    file: &str,
    revision: &str,
) -> Result<usize> {
    let mut reader = Nodes::open(input)?;
    let mut builder = crate::document::nodes::Builder::new(config)?;
    let mut offset = 0;
    while offset < count {
        let batch = reader.take(&json!({"offset":offset,"limit":(count-offset).min(512)}))?;
        let batch = batch.as_array().ok_or(INVALID)?;
        if batch.is_empty() {
            return Err(INVALID);
        }
        for node in batch {
            builder.push(node.clone(), offset as usize)?;
            offset += 1;
        }
    }
    reader.finished()?;
    let prepared = builder.finish(
        dataset,
        file,
        revision,
        config["window"].as_u64().unwrap_or(4).max(1) as usize,
    )?;
    let count = prepared.len();
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|_| "INDEX_SPOOL_WRITE_FAILED")?;
    let mut writer = BufWriter::new(file);
    let mut bytes = 0;
    for node in prepared {
        let line = serde_json::to_vec(&node).map_err(|_| INVALID)?;
        if line.len() + 1 > 8 * 1024 * 1024 {
            return Err("INDEX_POINT_SIZE_LIMIT");
        }
        bytes += line.len() + 1;
        if bytes > 512 * 1024 * 1024 {
            return Err("INDEX_FILE_CHUNK_LIMIT");
        }
        writer
            .write_all(&line)
            .and_then(|_| writer.write_all(b"\n"))
            .map_err(|_| "INDEX_SPOOL_WRITE_FAILED")?;
    }
    writer
        .flush()
        .and_then(|_| writer.get_ref().sync_all())
        .map_err(|_| "INDEX_SPOOL_WRITE_FAILED")?;
    Ok(count)
}
pub(super) struct Nodes {
    reader: BufReader<File>,
    offset: usize,
    bytes: u64,
}
impl Nodes {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let file = options.open(path).map_err(|_| "INDEX_NODES_UNAVAILABLE")?;
        if !file.metadata().map_err(|_| INVALID)?.is_file() {
            return Err(INVALID);
        }
        Ok(Self {
            reader: BufReader::new(file),
            offset: 0,
            bytes: 0,
        })
    }
    pub(super) fn take(&mut self, request: &Value) -> Result<Value> {
        if request["offset"].as_u64() != Some(self.offset as u64) {
            return Err(INVALID);
        }
        let limit = request["limit"]
            .as_u64()
            .filter(|v| *v > 0 && *v <= 512)
            .ok_or(INVALID)?;
        let mut nodes = Vec::new();
        let mut total = 0;
        for _ in 0..limit {
            let mut line = Vec::new();
            let n = (&mut self.reader)
                .take(8 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|_| "INDEX_NODES_UNAVAILABLE")?;
            if n == 0 {
                break;
            }
            if n > 8 * 1024 * 1024 || !line.ends_with(b"\n") {
                return Err("INDEX_POINT_SIZE_LIMIT");
            }
            self.bytes += n as u64;
            if self.bytes > 512 * 1024 * 1024 {
                return Err("INDEX_FILE_CHUNK_LIMIT");
            }
            total += n;
            nodes.push(serde_json::from_slice::<Value>(&line).map_err(|_| INVALID)?);
            self.offset += 1;
            if total >= 16 * 1024 * 1024 {
                break;
            }
        }
        Ok(json!(nodes))
    }
    pub(super) fn finished(&mut self) -> Result<()> {
        if !self.reader.fill_buf().map_err(|_| INVALID)?.is_empty() {
            Err(INVALID)
        } else {
            Ok(())
        }
    }
}
pub(super) fn fingerprint(path: &str) -> Result<Value> {
    let value =
        crate::files::fingerprint(&json!({"path":path,"include_mtime":true})).map_err(|code| {
            match code {
                "CORE_FILE_CHANGED" => "SOURCE_CHANGED_DURING_INDEXING",
                "CORE_FILE_UNAVAILABLE" | "CORE_NOT_REGULAR_FILE" => "INDEX_SOURCE_UNAVAILABLE",
                other => other,
            }
        })?;
    Ok(json!({"file_hash":value["sha256"],"file_mtime":value["mtime"],"file_size":value["size"]}))
}
