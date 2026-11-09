//! Read a saved mail file without touching the mailbox or writing its MIME parts.
use super::Result;
use mail_parser::{decoders::html::html_to_text, Address, MessageParser, MimeHeaders};
use serde_json::{json, Value};

fn emlx(bytes: &[u8]) -> Result<&[u8]> {
    let end = bytes
        .iter()
        .take(32)
        .position(|v| *v == b'\n')
        .ok_or("MAIL_EMLX_INVALID")?;
    let length = std::str::from_utf8(&bytes[..end])
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .ok_or("MAIL_EMLX_INVALID")?;
    let start = end + 1;
    bytes
        .get(start..start.checked_add(length).ok_or("MAIL_EMLX_INVALID")?)
        .ok_or("MAIL_EMLX_INVALID")
}
pub(super) fn label(value: &str, size: usize) -> String {
    value
        .chars()
        .map(|v| if v.is_control() { ' ' } else { v })
        .take(size)
        .collect()
}
fn addresses(value: Option<&Address>) -> String {
    value
        .into_iter()
        .flat_map(|v| v.iter())
        .take(40)
        .map(|entry| {
            let name = label(entry.name.as_deref().unwrap_or(""), 160);
            let addr = label(entry.address.as_deref().unwrap_or(""), 300);
            match (name.is_empty(), addr.is_empty()) {
                (false, false) => format!("{name} <{addr}>"),
                (false, true) => name,
                _ => addr,
            }
        })
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn append_attachment(
    text: &mut String,
    attachments: &mut Vec<Value>,
    name: &str,
    bytes: Option<&[u8]>,
    text_mime: bool,
    html_mime: bool,
) -> bool {
    let name = label(
        if name.is_empty() {
            "(без имени)"
        } else {
            name
        },
        250,
    );
    let Some(bytes) = bytes else {
        text.push_str(&format!(
            "Вложение: {name}\nСодержимое вложения не прочитано.\n\n"
        ));
        attachments.push(json!({"name":name,"size_bytes":Value::Null,"sha256":Value::Null,"status":"metadata_only"}));
        return false;
    };
    let suffix = name
        .rsplit_once('.')
        .map(|(_, v)| v.to_ascii_lowercase())
        .unwrap_or_default();
    let html = html_mime || matches!(suffix.as_str(), "html" | "htm");
    let extracted = if bytes.len() > 1024 * 1024 {
        None
    } else if suffix == "docx" {
        super::super::docx::read_bytes(bytes)
    } else if text_mime || html || matches!(suffix.as_str(), "txt" | "csv" | "md") {
        super::super::plain_text::decode(bytes).map(|v| if html { html_to_text(&v) } else { v })
    } else {
        None
    };
    let extracted =
        extracted.filter(|v| !v.trim().is_empty() && !v.contains('\0') && v.len() <= 1024 * 1024);
    text.push_str(&format!("Вложение: {name} ({} байт)\n", bytes.len()));
    if let Some(v) = &extracted {
        text.push_str(v.trim());
        text.push_str("\n\n");
    } else {
        text.push_str("Содержимое вложения не прочитано.\n\n");
    }
    attachments.push(
        json!({"name":name,"size_bytes":bytes.len(),"sha256":crate::sha256_hex(bytes),
        "status":if extracted.is_some(){"read"}else{"metadata_only"}}),
    );
    extracted.is_some()
}

pub(crate) fn read(bytes: &[u8], apple: bool) -> Result<Value> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("MAIL_FILE_LIMIT");
    }
    let raw = if apple { emlx(bytes)? } else { bytes };
    let message = MessageParser::default()
        .parse(raw)
        .ok_or("MAIL_MESSAGE_INVALID")?;
    if message.text_body_count() > 64 || message.attachment_count() > 100 {
        return Err("MAIL_PART_LIMIT");
    }
    let subject = label(message.subject().unwrap_or("Без темы"), 600);
    let from = addresses(message.from());
    let to = addresses(message.to());
    let date = message.date().map(|v| v.to_rfc3339()).unwrap_or_default();
    let mut text = format!("Тема: {subject}\nОт: {from}\nКому: {to}\nДата: {date}\n\n");
    let mut has_content = false;
    for index in 0..message.text_body_count() {
        if let Some(part) = message.body_text(index).filter(|v| !v.trim().is_empty()) {
            text.push_str(part.trim());
            text.push_str("\n\n");
            has_content = true;
        }
    }
    if !has_content {
        if let Some(part) = message
            .body_html(0)
            .map(|v| html_to_text(&v))
            .filter(|v| !v.trim().is_empty())
        {
            text.push_str(part.trim());
            text.push_str("\n\n");
            has_content = true;
        }
    }
    let mut attachments = Vec::new();
    for part in message.attachments() {
        let kind = part.content_type().map(|v| v.c_type.as_ref()).unwrap_or("");
        let html = part.content_type().and_then(|v| v.c_subtype.as_deref()) == Some("html");
        has_content |= append_attachment(
            &mut text,
            &mut attachments,
            part.attachment_name().unwrap_or(""),
            Some(part.contents()),
            kind == "text",
            html,
        );
    }
    if !has_content && from.is_empty() && message.subject().is_none() {
        return Err("MAIL_MESSAGE_INVALID");
    }
    Ok(
        json!({"kind":"mail","text":text,"metadata":{"extractor":"les.mail-mime.v1","attachments":attachments,"subject":subject,"sender":from,"recipients":to,"date":date,"message_id":message.message_id().unwrap_or_default(),"thread_id":message.in_reply_to().as_text().unwrap_or_default()}}),
    )
}
