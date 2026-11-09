//! Accept the existing local Outlook JSON push as one private MIME snapshot.
use super::{Result, BAD};
use crate::Context;
use les_core::catalog::mail;
use serde_json::{json, Value};
use std::fmt::Write;

fn header(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(900)
        .collect()
}

pub(super) fn register(context: &Context, raw: &[u8]) -> Result<Value> {
    let input: Value = serde_json::from_slice(raw).map_err(|_| BAD)?;
    if !input.is_object() {
        return Err(BAD);
    }
    let body = input["body"].as_str().unwrap_or_default();
    let attachments = match input.get("attachments") {
        None | Some(Value::Null) => &[][..],
        Some(value) => value
            .as_array()
            .filter(|v| v.len() <= 100)
            .ok_or(BAD)?
            .as_slice(),
    };
    let boundary = format!("les-{}", les_core::sha256_hex(raw));
    if body.contains(&boundary) {
        return Err(BAD);
    }
    let mut mime = format!(
        "Subject: {}\r\nFrom: {}\r\nDate: {}\r\nMIME-Version: 1.0\r\n",
        header(&input["subject"]),
        header(&input["from"]),
        header(&input["date"])
    );
    write!(mime,"Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n").map_err(|_| BAD)?;
    for item in attachments {
        let name = header(&item["name"]);
        let encoded = item["content_b64"].as_str().ok_or(BAD)?;
        let data = encoded.trim_end_matches('=');
        if name.is_empty()
            || data.is_empty()
            || encoded.len() % 4 != 0
            || encoded.len() - data.len() > 2
            || !data
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
        {
            return Err(BAD);
        }
        let filename =
            percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
        write!(mime,"--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename*=UTF-8''{filename}\r\nContent-Transfer-Encoding: base64\r\n\r\n").map_err(|_| BAD)?;
        for line in encoded.as_bytes().chunks(76) {
            mime.push_str(std::str::from_utf8(line).map_err(|_| BAD)?);
            mime.push_str("\r\n");
        }
        if mime.len() > mail::MAX_MESSAGE {
            return Err(BAD);
        }
    }
    write!(mime, "--{boundary}--\r\n").map_err(|_| BAD)?;
    if mime.len() > mail::MAX_MESSAGE {
        return Err(BAD);
    }
    let account = mail::create(
        &context.state_root,
        &context.database,
        &json!({"kind":"archive","label":"Входящие из Outlook","native_account_id":"outlook-push"}),
        true,
    )?;
    let id = account["id"].as_str().ok_or(BAD)?;
    let message = mail::register(
        &context.state_root,
        id,
        "eml",
        mime.as_bytes(),
        &json!({"parse":true}),
    )?;
    crate::intake::wake(context);
    Ok(
        json!({"ok":true,"status":"registered","message_id":message["id"],"dataset_id":account["dataset_id"],
        "account_id":id,"attachments":attachments.len()}),
    )
}
