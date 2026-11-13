//! Report the failing reader stage, rather than guessing from features present in a file.
use super::*;
pub(super) fn unsupported(name: &str) -> &'static str {
    eprintln!(
        "{}",
        json!({"event":"docx_unsupported_element","element":name})
    );
    element_code(name)
}

pub(super) fn element_code(name: &str) -> &'static str {
    match name {
        "w:drawing" | "w:pict" | "mc:AlternateContent" => "DOCX_DRAWING_UNSUPPORTED",
        "w:object" => "DOCX_EMBEDDED_OBJECT_UNSUPPORTED",
        "w:ins" | "w:del" | "w:moveFrom" | "w:moveTo" | "w:pPrChange" | "w:rPrChange"
        | "w:tblPrChange" | "w:tcPrChange" | "w:trPrChange" => "DOCX_REVISION_UNSUPPORTED",
        "w:sym" => "DOCX_SYMBOL_UNSUPPORTED",
        "w:tbl" | "w:tr" | "w:tc" | "w:gridCol" => "DOCX_TABLE_UNSUPPORTED",
        s if s.starts_with("m:") => "DOCX_EQUATION_UNSUPPORTED",
        _ => "DOCX_READ_FAILED",
    }
}

pub fn checked(bytes: &[u8]) -> Result<String, &'static str> {
    checked_with(bytes, &mut TextParts)
}
pub(super) fn checked_with(bytes: &[u8], parts: &mut dyn Parts) -> Result<String, &'static str> {
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("DOCUMENT_FILE_LIMIT");
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
    if archive.has_duplicate_names() {
        return Err("DOCUMENT_ARCHIVE_INVALID");
    }
    if archive.len() > 10_000 {
        return Err("DOCUMENT_ARCHIVE_LIMIT");
    }
    let mut names = std::collections::HashSet::new();
    for index in 0..archive.len() {
        let member = archive
            .by_index(index)
            .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
        let name = member.name().map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
        if !names.insert(name.into_owned()) {
            return Err("DOCUMENT_ARCHIVE_INVALID");
        }
    }
    let levels = styles::read(&mut archive).ok_or("DOCX_READ_FAILED")?;
    let numbering = numbering::Numbering::read(&mut archive).ok_or("DOCX_NUMBERING_UNSUPPORTED")?;
    let member = archive
        .by_name("word/document.xml")
        .map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?;
    if member.size() > MAX_MAIN_XML {
        return Err("DOCUMENT_XML_LIMIT");
    }
    parse_stream(
        BufReader::new(member.take(MAX_MAIN_XML + 1)),
        levels,
        numbering,
        parts,
    )
}
