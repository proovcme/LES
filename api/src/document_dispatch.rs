//! The native format worker owns PDF extraction and the complete source -> node dispatch.
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn configured_ocr(owned_state: Option<&Path>) -> Result<Value, &'static str> {
    let root = std::env::var_os("LES_RUNTIME_HOME").ok_or("OCR_UNAVAILABLE")?;
    let state = owned_state
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("LES_STATE_ROOT").map(std::path::PathBuf::from))
        .ok_or("OCR_UNAVAILABLE")?;
    let root = Path::new(&root);
    let state = state.as_path();
    if !root.is_absolute() || !state.is_absolute() {
        return Err("OCR_UNAVAILABLE");
    }
    let file = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.join(".env"));
    let values = les_core::model_secrets::file_values(&file)?;
    let setting = |key: &str, default: &str| {
        std::env::var(key)
            .ok()
            .or_else(|| values.get(key).cloned())
            .map(|value| {
                let value = value.trim();
                if value.len() >= 2
                    && ((value.starts_with('\'') && value.ends_with('\''))
                        || (value.starts_with('"') && value.ends_with('"')))
                {
                    value[1..value.len() - 1].to_owned()
                } else {
                    value.to_owned()
                }
            })
            .unwrap_or_else(|| default.into())
    };
    let backend = setting(
        "RAG_OCR_BACKEND",
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            "windows"
        },
    );
    let (engine, helper) = match backend.trim().to_ascii_lowercase().as_str() {
        "windows" if cfg!(windows) => ("windows", root.join("tools/light_windows_ocr.ps1")),
        "macos" | "vision" if cfg!(target_os = "macos") => {
            ("vision", root.join("native/vision/les-ocr"))
        }
        "tesseract" => (
            "tesseract",
            std::path::PathBuf::from(setting("RAG_OCR_TESSERACT_BIN", "")),
        ),
        _ => return Err("OCR_UNAVAILABLE"),
    };
    let scratch = state.join("storage/ocr-tmp");
    std::fs::create_dir_all(&scratch).map_err(|_| "OCR_READ_FAILED")?;
    crate::pdf_ocr::cleanup(state);
    let dpi = setting(
        if engine == "tesseract" {
            "RAG_OCR_TESSERACT_DPI"
        } else {
            "RAG_OCR_DPI"
        },
        "300",
    )
    .parse::<u64>()
    .map_err(|_| "OCR_UNAVAILABLE")?
    .clamp(if engine == "windows" { 150 } else { 1 }, 2400);
    Ok(
        json!({"engine":engine,"helper":helper,"scratch":scratch,"dpi":dpi,
        "language":setting("RAG_OCR_TESSERACT_LANG","rus+eng"),"psm":setting("RAG_OCR_TESSERACT_PSM","6").parse::<u64>().map_err(|_|"OCR_UNAVAILABLE")?}),
    )
}

pub(crate) fn freeze_ocr(options: &mut Value, state: &Path) {
    if options["ocr"].is_object() {
        return;
    }
    match configured_ocr(Some(state)) {
        Ok(value) => options["ocr"] = value,
        Err(_) => options["ocr_unavailable"] = json!(true),
    }
}

pub(crate) fn external(request: &Value) -> Result<Value, &'static str> {
    let path = Path::new(request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?);
    if request["word_parser"] == "docling"
        && path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("doc"))
    {
        return crate::document_parser::legacy_word(request);
    }
    let library = std::env::var_os("LES_NATIVE_PDFIUM").ok_or("PDFIUM_UNAVAILABLE")?;
    let mut options = request.clone();
    options["kind"] = json!("pdf_text");
    options
        .as_object_mut()
        .ok_or("DOCUMENT_DISPATCH_INVALID")?
        .remove("ocr");
    let mut value = crate::pdf_text::convert(Path::new(&library), path, &options)?;
    if value["needs_ocr"] == true {
        options["ocr"] = if request["ocr"].is_object() {
            request["ocr"].clone()
        } else if request["ocr_unavailable"] == true {
            return Err("OCR_UNAVAILABLE");
        } else {
            configured_ocr(request["owned_state"].as_str().map(Path::new))?
        };
        options["kind"] = json!("pdf_ocr");
        value = crate::pdf_text::convert(Path::new(&library), path, &options)?;
    }
    if value["text"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return Err("NO_SEARCHABLE_TEXT");
    }
    Ok(
        json!({"kind":"pdf","text":value["text"],"metadata":{"page_count":value["pages"],"pdf_page_views":value["page_views"],"ocr":value["ocr"],"extractor":"les.pdfium.layout.v2"}}),
    )
}

pub fn serve() -> Result<(), &'static str> {
    let docling = crate::document_parser::enabled()?;
    les_core::serve_document_format_requests(&|input| {
        let mut input = input.clone();
        if docling {
            input["word_parser"] = json!("docling");
        }
        les_core::convert_document_with_parts(&input, &|input, part| match part {
            Some(bytes) if input["part_kind"] == "word_structure" => {
                crate::document_parser::word(input, bytes)
            }
            Some(bytes) => crate::document_media::read(input, bytes),
            None => external(input),
        })
    })
}
