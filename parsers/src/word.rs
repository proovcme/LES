use super::*;
use docling::{DocumentConverter, ImageSources, SourceDocument};
mod structure;

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
    // One projection of the canonical tree, retaining raw runs and logical
    // cells/spans. Never render styled Markdown and then infer its structure.
    // The pinned binary .doc backend has flat nodes, not an ItemTree. Keep its
    // explicit legacy adapter instead of falsely labelling Markdown as an AST
    // or removing a supported format. DOCX must always have the canonical tree.
    let mut result = if extension == "doc" && converted.document.tree.is_none() {
        let (text, artifacts) =
            converted
                .document
                .export_to_markdown_with_options(&docling::MarkdownExportOptions {
                    layers: docling::ContentLayers::ALL,
                    traverse_pictures: true,
                    escape_underscores: false,
                    ..Default::default()
                });
        if !artifacts.is_empty() {
            return Err("DOCLING_WORD_INCOMPLETE");
        }
        if text.len() > super::MAX_OUTPUT - 65536 {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
        if text.trim().is_empty() {
            return Err("NO_SEARCHABLE_TEXT");
        }
        json!({"text":text})
    } else {
        let structure = structure::project(&converted.document)?;
        if !crate::canonical::has_text(&structure, None) {
            return Err("NO_SEARCHABLE_TEXT");
        }
        // Validate span expansion limits, then discard presentation. Only the
        // typed structure crosses IPC; the host renders after native restoration.
        drop(crate::canonical::render(&structure)?);
        json!({"structure":structure})
    };
    drop(converted); // Release the original tree/flat nodes before IPC serialization.
    result["extractor"] = json!("docling.rs.2.82.1.word.v2");
    result["ocr"] = json!(false);
    result["source_sha256"] = input["sha256"].clone();
    Ok(result)
}
