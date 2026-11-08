//! The native format worker owns PDF extraction and the complete source -> node dispatch.
use serde_json::{json, Value};
use std::path::Path;

fn configured_ocr(owned_state: Option<&Path>) -> Result<Value, &'static str> {
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
    let dpi = setting(
        if engine == "tesseract" {
            "RAG_OCR_TESSERACT_DPI"
        } else {
            "RAG_OCR_DPI"
        },
        if engine == "tesseract" { "300" } else { "150" },
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

fn external(request: &Value) -> Result<Value, &'static str> {
    let path = Path::new(request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?);
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
            configured_ocr(None)?
        };
        options["kind"] = json!("pdf_ocr");
        value = crate::pdf_text::convert(Path::new(&library), path, &options)?;
    }
    Ok(
        json!({"kind":"pdf","text":value["text"],"metadata":{"page_count":value["pages"],"ocr":value["ocr"],"extractor":"les.pdfium.v1"}}),
    )
}

pub fn serve() -> Result<(), &'static str> {
    les_core::serve_document_formats_with(&external)
}
