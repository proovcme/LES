//! No OCR, model hub, URL fetching or indexing. The LES host owns those policies.
mod pdf;
mod word;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};
type Result<T> = std::result::Result<T, &'static str>;
const MAX_OUTPUT: usize = 24 * 1024 * 1024;

fn source(input: &Value, limit: u64) -> Result<Vec<u8>> {
    let path = Path::new(input["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?);
    if !path.is_absolute() {
        return Err("DOCUMENT_PATH_INVALID");
    }
    let file = File::open(path).map_err(|_| "DOCUMENT_READ_FAILED")?;
    let info = file.metadata().map_err(|_| "DOCUMENT_READ_FAILED")?;
    if !info.is_file() || info.len() > limit {
        return Err("DOCUMENT_FILE_LIMIT");
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "DOCUMENT_READ_FAILED")?;
    if bytes.len() as u64 > limit {
        return Err("DOCUMENT_FILE_LIMIT");
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if input["sha256"].as_str() != Some(hash.as_str()) {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    Ok(bytes)
}

fn convert(input: &Value, rt: &tokio::runtime::Runtime) -> Result<Value> {
    if input["schema"] != "les.document-parsers.v1" {
        return Err("DOCUMENT_PARSER_SCHEMA_INVALID");
    }
    match input["action"].as_str() {
        Some("word") => word::convert(input, source(input, 160 * 1024 * 1024)?),
        Some("pdf_tables") => pdf::convert(input, source(input, 512 * 1024 * 1024)?, rt),
        _ => Err("DOCUMENT_PARSER_ACTION_INVALID"),
    }
}
fn serve() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "DOCUMENT_PARSER_START_FAILED")?;
    let mut reader = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = Vec::new();
        if reader
            .by_ref()
            .take(65537)
            .read_until(b'\n', &mut line)
            .map_err(|_| "DOCUMENT_INPUT_FAILED")?
            == 0
        {
            return Ok(());
        }
        if line.len() > 65536 || !line.ends_with(b"\n") {
            return Err("DOCUMENT_INPUT_LIMIT");
        }
        let envelope: Value =
            serde_json::from_slice(&line).map_err(|_| "DOCUMENT_INPUT_INVALID")?;
        let result = match convert(&envelope["input"], &rt) {
            Ok(v) => json!({"status":"completed","value":v}),
            Err(code) => {
                eprintln!("{}", json!({"event":"document_parser_failed","code":code}));
                json!({"status":"failed","code":code})
            }
        };
        let mut bytes =
            serde_json::to_vec(&json!({"request_id":envelope["request_id"],"result":result}))
                .map_err(|_| "DOCUMENT_OUTPUT_FAILED")?;
        if bytes.len() > MAX_OUTPUT {
            bytes=serde_json::to_vec(&json!({"request_id":envelope["request_id"],"result":{"status":"failed","code":"DOCUMENT_TEXT_LIMIT"}})).unwrap();
        }
        output
            .write_all(&bytes)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .map_err(|_| "DOCUMENT_OUTPUT_FAILED")?;
    }
}
fn main() -> Result<()> {
    std::thread::Builder::new()
        .name("les-document-parsers".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(serve)
        .map_err(|_| "DOCUMENT_PARSER_START_FAILED")?
        .join()
        .map_err(|_| "DOCUMENT_PARSER_PANICKED")?
}
