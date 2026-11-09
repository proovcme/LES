//! Read-only, bounded format conversion. The worker owns parsing; callers own indexing.
mod mail;
mod pipeline;
mod presentation;
mod spreadsheet;
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

type Result<T> = std::result::Result<T, &'static str>;
pub(crate) fn csv_snapshot(path: &Path) -> Result<Value> {
    spreadsheet::snapshot_csv(&bytes(path)?)
}
const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_TEXT: usize = 24 * 1024 * 1024;
pub(crate) fn supports(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "xlsx"
                | "xlsm"
                | "xls"
                | "csv"
                | "pptx"
                | "docx"
                | "doc"
                | "txt"
                | "md"
                | "json"
                | "jsonl"
                | "pdf"
        )
    })
}

fn bytes(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        return Err("DOCUMENT_PATH_INVALID");
    }
    let file = File::open(path).map_err(|_| "DOCUMENT_READ_FAILED")?;
    let metadata = file.metadata().map_err(|_| "DOCUMENT_READ_FAILED")?;
    if !metadata.is_file() {
        return Err("DOCUMENT_READ_FAILED");
    }
    let limit = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        512 * 1024 * 1024
    } else {
        MAX_FILE
    };
    if metadata.len() > limit {
        return Err("DOCUMENT_FILE_LIMIT");
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "DOCUMENT_READ_FAILED")?;
    if bytes.len() as u64 > limit {
        return Err("DOCUMENT_FILE_LIMIT");
    }
    Ok(bytes)
}

fn archive(bytes: &[u8]) -> Result<zip::ZipArchive<std::io::Cursor<&[u8]>>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
    if archive.len() > 10_000 {
        return Err("DOCUMENT_ARCHIVE_LIMIT");
    }
    let mut remaining = 128 * 1024 * 1024;
    let mut names = std::collections::HashSet::new();
    for index in 0..archive.len() {
        let item = archive
            .by_index(index)
            .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
        if !names.insert(
            item.name()
                .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?
                .into_owned(),
        ) || item.enclosed_name().is_none()
        {
            return Err("DOCUMENT_ARCHIVE_INVALID");
        }
        let size = item.size();
        if size > remaining {
            return Err("DOCUMENT_ARCHIVE_LIMIT");
        }
        let count = std::io::copy(&mut item.take(size + 1), &mut std::io::sink())
            .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
        if count != size {
            return Err("DOCUMENT_ARCHIVE_INVALID");
        }
        remaining -= size;
    }
    Ok(archive)
}

fn xml(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Result<String> {
    let mut item = archive
        .by_name(name)
        .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
    if item.size() > 16 * 1024 * 1024 {
        return Err("DOCUMENT_XML_LIMIT");
    }
    let mut text = String::new();
    item.read_to_string(&mut text)
        .map_err(|_| "DOCUMENT_XML_INVALID")?;
    Ok(text)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('|', "&#124;")
        .replace('\r', "")
        .replace('\n', "<br>")
}

pub(crate) fn execute(request: &Value) -> Result<Value> {
    execute_with(request, &|_| Err("DOCUMENT_NATIVE_READER_REQUIRED"))
}

pub fn execute_with(request: &Value, external: &impl Fn(&Value) -> Result<Value>) -> Result<Value> {
    if request["schema"] != "les.document-convert.v1" {
        return Err("DOCUMENT_CONVERT_SCHEMA_INVALID");
    }
    let path = Path::new(request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?);
    let suffix = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Refuse unknown formats before opening them, without a second hidden parser.
    let mail = matches!(suffix.as_str(), "eml" | "emlx");
    if !supports(path) && !mail {
        return Err("DOCUMENT_CONVERT_UNSUPPORTED");
    }
    if mail && request["pipeline"] == "nodes" {
        // Generic folder publication does not yet enforce the mailbox egress policy.
        return Err("DOCUMENT_NATIVE_DISPATCH_REQUIRED");
    }
    let bytes = bytes(path)?;
    let source_sha256 = crate::sha256_hex(&bytes);
    if request
        .get("source_sha256")
        .is_some_and(|v| v.as_str() != Some(source_sha256.as_str()))
    {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    let table = matches!(suffix.as_str(), "xlsx" | "xlsm" | "xls" | "csv");
    let mut result = match suffix.as_str() {
        "eml" | "emlx" => mail::read(&bytes, suffix == "emlx")?,
        "pptx" if request["pipeline"] != "nodes" => presentation::read(&bytes)?,
        "pptx" => Value::Null,
        "docx" => {
            json!({"text":super::docx::read_bytes(&bytes).ok_or("DOCX_READ_FAILED")?,"metadata":{"extractor":"les.docx-paragraphs.v1"}})
        }
        "doc" => {
            json!({"text":super::legacy_doc::read(path)?,"metadata":{"extractor":"les.ms-doc.v1"}})
        }
        "txt" | "md" => {
            let text = super::plain_text::decode(&bytes).ok_or("DOCUMENT_ENCODING_REQUIRED")?;
            if text
                .chars()
                .any(|c| c < ' ' && !matches!(c, '\t' | '\r' | '\n' | '\u{c}'))
            {
                return Err("DOCUMENT_BINARY_TEXT");
            }
            json!({"text":text,"metadata":{"extractor":"les.unicode.v1"}})
        }
        "json" | "jsonl" => {
            let text = super::plain_text::decode(&bytes).ok_or("DOCUMENT_ENCODING_REQUIRED")?;
            let value = super::json_source::convert(
                &json!({"json_lines":suffix=="jsonl","max_chars":MAX_TEXT}),
                &text,
            )?;
            if value["status"] != "completed" {
                return Err(value["code"]
                    .as_str()
                    .and_then(json_error)
                    .unwrap_or("JSON_READ_FAILED"));
            }
            json!({"text":value["text"],"metadata":{"extractor":value["extractor"],"truncated":value["truncated"],"records":value["records"]}})
        }
        "pdf" => {
            let mut input = request.clone();
            input["source_sha256"] = json!(source_sha256);
            external(&input)?
        }
        _ if request["pipeline"] == "nodes" => Value::Null,
        _ => spreadsheet::read(&bytes, &suffix, path, request)?,
    };
    // Path-based adapters must prove that they read the same registered revision.
    if matches!(suffix.as_str(), "doc" | "pdf")
        && crate::sha256_hex(&self::bytes(path)?) != source_sha256
    {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    if request["pipeline"] == "nodes" {
        return pipeline::prepare(
            request,
            &bytes,
            path,
            &suffix,
            &source_sha256,
            (!table && suffix != "pptx").then_some(&result),
        );
    }
    if result["text"]
        .as_str()
        .is_none_or(|text| text.len() > MAX_TEXT)
    {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    result["source_sha256"] = json!(source_sha256);
    result["owner"] = json!("rust.document.conversion");
    result["status"] = json!("completed");
    if serde_json::to_vec(&result)
        .map_err(|_| "DOCUMENT_OUTPUT_FAILED")?
        .len()
        > 30 * 1024 * 1024
    {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    Ok(result)
}

fn json_error(code: &str) -> Option<&'static str> {
    [
        "JSON_INVALID",
        "JSON_READ_FAILED",
        "JSON_RECORD_LIMIT",
        "NO_SEARCHABLE_TEXT",
        "JSON_DEPTH_LIMIT",
        "JSON_NUMBER_INVALID",
    ]
    .into_iter()
    .find(|v| *v == code)
}

pub fn serve() -> Result<()> {
    serve_with(&|_| Err("DOCUMENT_NATIVE_READER_REQUIRED"))
}
pub fn serve_with(external: &impl Fn(&Value) -> Result<Value>) -> Result<()> {
    serve_requests(&|input| execute_with(input, external))
}
pub(crate) fn serve_requests(handler: &impl Fn(&Value) -> Result<Value>) -> Result<()> {
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    loop {
        let mut frame = Vec::new();
        if input
            .by_ref()
            .take(8 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut frame)
            .map_err(|_| "DOCUMENT_INPUT_FAILED")?
            == 0
        {
            return Ok(());
        }
        if frame.len() > 8 * 1024 * 1024 || !frame.ends_with(b"\n") {
            return Err("DOCUMENT_INPUT_LIMIT");
        }
        let envelope: Value =
            serde_json::from_slice(&frame).map_err(|_| "DOCUMENT_INPUT_INVALID")?;
        let result = match handler(&envelope["input"]) {
            Ok(value) => json!({"status":"completed","value":value}),
            Err(code) => {
                eprintln!(
                    "{}",
                    json!({"event":"document_conversion_failed","code":code})
                );
                json!({"status":"failed","code":code})
            }
        };
        serde_json::to_writer(
            &mut output,
            &json!({"request_id":envelope["request_id"],"result":result}),
        )
        .map_err(|_| "DOCUMENT_OUTPUT_FAILED")?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|_| "DOCUMENT_OUTPUT_FAILED")?;
    }
}
