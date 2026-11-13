use super::*;
use xberg::{extract, ExtractInput, ExtractionConfig};

pub(super) fn convert(
    input: &Value,
    bytes: Vec<u8>,
    rt: &tokio::runtime::Runtime,
) -> Result<Value> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("PDF_OPEN_FAILED");
    }
    let config:ExtractionConfig=serde_json::from_value(json!({"use_cache":false,"enable_quality_processing":false,
        "disable_ocr":true,"include_document_structure":false,
        "images":{"extract_images":false,"run_ocr_on_images":false},
        "pages":{"extract_pages":false,"insert_page_markers":false},
        "content_filter":{"include_headers":true,"include_footers":true,"strip_repeating_text":false}})).map_err(|_|"DOCUMENT_PARSER_CONFIG_INVALID")?;
    let output = rt
        .block_on(Box::pin(extract(
            ExtractInput::from_bytes(bytes, "application/pdf", Some("source.pdf".into())),
            &config,
        )))
        .map_err(|_| "XBERG_PDF_TABLES_FAILED")?;
    if output.results.len() != 1 {
        return Err("XBERG_PDF_TABLES_INVALID");
    }
    let doc = &output.results[0];
    let meta = serde_json::to_value(&doc.metadata).map_err(|_| "XBERG_PDF_TABLES_INVALID")?;
    let count = meta["format"]["page_count"]
        .as_u64()
        .or_else(|| meta["pages"]["total_count"].as_u64())
        .ok_or("PDF_PAGE_LIMIT")?;
    if count > 10000 {
        return Err("PDF_PAGE_LIMIT");
    }
    let mut cells = 0_usize;
    let mut characters = 0_usize;
    let mut tables = Vec::new();
    for t in &doc.tables {
        if t.page_number == 0 || t.page_number as u64 > count {
            return Err("XBERG_PDF_TABLES_INVALID");
        }
        for row in &t.cells {
            cells += row.len();
            for c in row {
                characters += c.len();
            }
        }
        if cells > 300000 || characters > 12 * 1024 * 1024 {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
        if t.cells.iter().flatten().any(|c| !c.trim().is_empty()) {
            tables.push(json!({"page":t.page_number,"cells":t.cells,"bbox":t.bounding_box}));
        }
    }
    Ok(
        json!({"page_count":count,"tables":tables,"extractor":"xberg.1.3.10.pdf-tables.v1","ocr":false,"source_sha256":input["sha256"]}),
    )
}
