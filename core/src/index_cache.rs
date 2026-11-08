//! Bounded on-disk transfer of cached Qdrant vectors between Rust processes.
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"LESCACH1";
const HASH_LEN: usize = 40;
const HEADER_LEN: usize = 12;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub fn is_hash(value: &str) -> bool {
    value.len() == HASH_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Called by the state owner before starting any index writers.
pub fn remove_abandoned(state: &Path) -> std::io::Result<usize> {
    let expected = std::fs::canonicalize(state)?.join("data/index-publish");
    let directory = match std::fs::canonicalize(&expected) {
        Ok(path) if path == expected => path,
        Ok(_) => return Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some((id, suffix)) = name.to_str().and_then(|name| name.split_once('.')) else {
            continue;
        };
        if id.len() == 32
            && id.bytes().all(|byte| byte.is_ascii_hexdigit())
            && matches!(suffix, "cache" | "jsonl")
            && entry.file_type()?.is_file()
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

pub struct CacheWriter {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    dimension: usize,
    bytes: usize,
    keep: bool,
}

impl CacheWriter {
    pub fn create(path: &Path, dimension: u32) -> std::io::Result<Self> {
        if !(1..=65_536).contains(&dimension) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut cache = Self {
            path: path.to_owned(),
            writer: Some(BufWriter::new(file)),
            dimension: dimension as usize,
            bytes: HEADER_LEN,
            keep: false,
        };
        let writer = cache.writer.as_mut().expect("cache writer open");
        writer.write_all(MAGIC)?;
        writer.write_all(&dimension.to_le_bytes())?;
        Ok(cache)
    }

    pub fn add(&mut self, hash: &str, vector: &[f32]) -> std::io::Result<bool> {
        if !is_hash(hash) || vector.len() != self.dimension {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let size = HASH_LEN + self.dimension * 4;
        if self.bytes + size > MAX_BYTES {
            return Ok(false);
        }
        let writer = self.writer.as_mut().expect("cache writer open");
        writer.write_all(hash.as_bytes())?;
        for number in vector {
            writer.write_all(&number.to_le_bytes())?;
        }
        self.bytes += size;
        Ok(true)
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        self.keep = true;
        Ok(())
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        self.writer.take();
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn load(path: &Path, dimension: usize) -> std::io::Result<HashMap<String, Vec<f32>>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_BYTES as u64 || dimension == 0 || dimension > 65_536 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut reader = BufReader::new(file);
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if &header[..8] != MAGIC
        || u32::from_le_bytes(header[8..].try_into().unwrap()) as usize != dimension
    {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut vectors = HashMap::new();
    let mut hash = [0; HASH_LEN];
    let mut encoded = vec![0; dimension * 4];
    while !reader.fill_buf()?.is_empty() {
        reader.read_exact(&mut hash)?;
        reader.read_exact(&mut encoded)?;
        let hash = std::str::from_utf8(&hash).map_err(|_| std::io::ErrorKind::InvalidData)?;
        if !is_hash(hash) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let vector: Vec<_> = encoded
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        if vector.iter().all(|number| number.is_finite()) {
            vectors.entry(hash.to_owned()).or_insert(vector);
        }
    }
    Ok(vectors)
}
