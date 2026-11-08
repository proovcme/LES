//! Read-only format adapters. No model, indexing or source writes.
pub(crate) mod chunks;
pub(crate) mod conversion;
mod docx;
mod embedding_chunks;
pub(crate) mod json_source;
mod legacy_doc;
mod node_hierarchy;
pub(crate) mod nodes;
mod plain_text;
pub(crate) mod source_nodes;
mod tokenizer;
pub(crate) mod workbook;
pub(crate) mod workbook_index;
use serde_json::{json, Value};

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    match request["action"].as_str() {
        Some("convert") => conversion::execute(request),
        Some("pdf_page_blocks") => source_nodes::page_blocks(request),
        Some("json_document") => json_source::read(request),
        Some("prepare_nodes") => nodes::execute(request),
        Some("chunk_text") if request["tokenizer"].is_object() => {
            let local = tokenizer::load(&request["tokenizer"])?;
            chunks::execute(request, Some(&|text| local.count(text)))
        }
        Some("chunk_text") => chunks::execute(request, None),
        Some("embedding_chunks") if request["tokenizer"].is_object() => {
            let local = tokenizer::load(&request["tokenizer"])?;
            embedding_chunks::execute(request, Some(&|text| local.count(text)))
        }
        Some("embedding_chunks") => embedding_chunks::execute(request, None),
        Some("tokenizer_info" | "token_count") => tokenizer::execute(request),
        Some("markdown_sections") => chunks::markdown_sections(request),
        Some("docx_text") => {
            let path = request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?;
            Ok(docx::read(std::path::Path::new(path)))
        }
        Some("plain_text") => {
            let path = request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?;
            plain_text::read(std::path::Path::new(path))
        }
        Some("legacy_doc") => {
            let path = request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?;
            let text = legacy_doc::read(std::path::Path::new(path))?;
            Ok(json!({"text":text,"extractor":"les.ms-doc.v1"}))
        }
        _ => Err("DOCUMENT_ACTION_INVALID"),
    }
}

pub(super) fn measure_chunks(
    request: &Value,
    measure: Option<&chunks::Measure<'_>>,
) -> Result<Value, &'static str> {
    if request["action"] == "embedding_chunks" {
        embedding_chunks::execute(request, measure)
    } else {
        chunks::execute(request, measure)
    }
}
