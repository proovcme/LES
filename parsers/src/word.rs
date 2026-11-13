use super::*;
use docling::{
    ContentLayers, DocumentConverter, ImageSources, MarkdownExportOptions, SourceDocument,
};
mod slots;

pub(super) fn convert(input: &Value, bytes: Vec<u8>) -> Result<Value> {
    let extension = input["format"]
        .as_str()
        .filter(|s| matches!(*s, "docx" | "doc"))
        .ok_or("DOCUMENT_PARSER_ACTION_INVALID")?;
    if extension == "docx" {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
        if archive.has_duplicate_names() || archive.len() > 10000 {
            return Err("DOCUMENT_ARCHIVE_INVALID");
        }
        let mut total = 0_u64;
        for index in 0..archive.len() {
            let item = archive
                .by_index(index)
                .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
            total = total
                .checked_add(item.size())
                .ok_or("DOCUMENT_ARCHIVE_LIMIT")?;
            if total > 160 * 1024 * 1024 || item.enclosed_name().is_none() {
                return Err("DOCUMENT_ARCHIVE_LIMIT");
            }
        }
    }
    let source = SourceDocument::from_named_bytes(&format!("source.{extension}"), bytes, None)
        .map_err(|_| "DOCLING_WORD_FAILED")?;
    let converted = DocumentConverter::new()
        .text_layer_only(true)
        .image_sources(ImageSources::None)
        .convert(source)
        .map_err(|_| "DOCLING_WORD_FAILED")?;
    if !converted.errors.is_empty()
        || !matches!(converted.status, docling::ConversionStatus::Success)
    {
        eprintln!(
            "{}",
            json!({"event":"docling_word_incomplete","stage":"conversion_status","status":format!("{:?}",converted.status),"errors":converted.errors.iter().map(|e|format!("{e:?}")).collect::<Vec<_>>()})
        );
        return Err("DOCLING_WORD_INCOMPLETE");
    }
    // Export the tree directly. Never materialize the enormous text+orig+cell JSON.
    let options = MarkdownExportOptions {
        layers: ContentLayers::ALL,
        traverse_pictures: true,
        escape_underscores: false,
        ..Default::default()
    };
    let (text, artifacts) = converted.document.export_to_markdown_with_options(&options);
    if !artifacts.is_empty() {
        eprintln!(
            "{}",
            json!({"event":"docling_word_incomplete","stage":"unexpected_artifacts","count":artifacts.len()})
        );
        return Err("DOCLING_WORD_INCOMPLETE");
    }
    if text.len() > super::MAX_OUTPUT - 65536 {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    if text.trim().is_empty() {
        return Err("NO_SEARCHABLE_TEXT");
    }
    let expansions = if let Some(prefix) = input["slot_prefix"].as_str() {
        slots::expansions(&converted.document, &text, prefix)?
    } else {
        json!({})
    };
    Ok(
        json!({"text":text,"slot_expansions":expansions,"extractor":"docling.rs.2.82.1.word.v1","ocr":false,"source_sha256":input["sha256"]}),
    )
}
