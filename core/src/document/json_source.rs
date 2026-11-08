//! JSON/JSONL source data, never searchable parser diagnostics.
use super::{chunks::trim, plain_text};
use serde_json::{json, Value};
use std::{fs::OpenOptions, io::Read, path::Path};

const MAX_SOURCE: u64 = 128 * 1024 * 1024;
const MAX_RECORDS: usize = 2000;
const NOTE: &str =
    "*Документ показан частично: текст или число записей ограничены при индексации.*\n\n";
const NUMBERS: &str = "*Нестандартные числовые значения показаны буквально.*\n\n";

fn failed(code: &str) -> Value {
    json!({"status":"failed","code":code})
}
fn normalized_numbers(input: &str) -> Option<String> {
    if !input.contains("NaN") && !input.contains("Infinity") {
        return None;
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let (mut quoted, mut escaped, mut changed) = (false, false, false);
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            out.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            quoted = true;
            out.push(byte);
            index += 1;
            continue;
        }
        let token = [
            b"-Infinity".as_slice(),
            b"Infinity".as_slice(),
            b"NaN".as_slice(),
        ]
        .into_iter()
        .find(|token| {
            bytes[index..].starts_with(token)
                && (index == 0
                    || bytes[index - 1].is_ascii_whitespace()
                    || b"[:,".contains(&bytes[index - 1]))
                && (index + token.len() == bytes.len()
                    || bytes[index + token.len()].is_ascii_whitespace()
                    || b",]}".contains(&bytes[index + token.len()]))
        });
        if let Some(token) = token {
            out.push(b'"');
            out.extend_from_slice(token);
            out.push(b'"');
            index += token.len();
            changed = true;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    changed.then(|| String::from_utf8(out).expect("only ASCII quotes added to UTF-8"))
}
fn parse(input: &str) -> Result<(Value, bool), serde_json::Error> {
    match serde_json::from_str(input) {
        Ok(value) => Ok((value, false)),
        Err(error) => {
            match normalized_numbers(input).and_then(|value| serde_json::from_str(&value).ok()) {
                Some(value) => Ok((value, true)),
                None => Err(error),
            }
        }
    }
}
fn parse_error(input: &str, error: &serde_json::Error, physical_line: Option<usize>) -> Value {
    let line = input
        .lines()
        .nth(error.line().saturating_sub(1))
        .unwrap_or("");
    let column = line
        .char_indices()
        .take_while(|(offset, _)| *offset < error.column().saturating_sub(1))
        .count()
        + 1;
    json!({"status":"failed","code":"JSON_INVALID","line":physical_line.unwrap_or(error.line()),"column":column})
}
struct Text {
    value: String,
    chars: usize,
    limit: usize,
    truncated: bool,
}
impl Text {
    fn put(&mut self, value: &str) {
        let count = value.chars().count();
        let remaining = self.limit.saturating_sub(self.chars);
        self.value.extend(value.chars().take(remaining));
        self.chars += count.min(remaining);
        self.truncated |= count > remaining;
    }
    fn full(&self) -> bool {
        self.chars >= self.limit
    }
}
fn priority(key: &str) -> usize {
    [
        "role",
        "user",
        "assistant",
        "system",
        "prompt",
        "question",
        "answer",
        "response",
        "title",
        "subject",
        "content",
        "message",
        "text",
        "description",
        "summary",
        "body",
        "delta",
    ]
    .iter()
    .position(|value| *value == key.to_lowercase())
    .unwrap_or(100)
}
fn render(value: &Value, out: &mut Text, depth: usize) -> Result<(), &'static str> {
    if depth > 64 {
        return Err("JSON_NESTING_LIMIT");
    }
    match value {
        Value::Object(fields) => {
            if fields.is_empty() {
                out.put("{}");
            }
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(key, _)| priority(key));
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if out.full() {
                    out.truncated = true;
                    break;
                }
                if index > 0 {
                    out.put("\n");
                }
                out.put("**");
                out.put(key);
                out.put(":** ");
                render(value, out, depth + 1)?;
            }
        }
        Value::Array(values) => {
            if values.is_empty() {
                out.put("[]");
            }
            for (index, value) in values.iter().enumerate() {
                if out.full() {
                    out.truncated = true;
                    break;
                }
                if index > 0 {
                    out.put("\n");
                }
                out.put(&format!("[{}] ", index + 1));
                render(value, out, depth + 1)?;
            }
        }
        Value::String(value) => {
            if value.is_empty() {
                out.put("(пустая строка)");
            } else {
                out.put(value);
            }
        }
        value => out.put(&value.to_string()),
    }
    Ok(())
}
fn record(value: &Value, number: usize, out: &mut Text) -> Result<bool, &'static str> {
    if value.as_object().is_some_and(|v| v.is_empty())
        || value.as_array().is_some_and(|v| v.is_empty())
    {
        return Ok(false);
    }
    if !out.value.is_empty() {
        out.put("\n\n");
    }
    out.put(&format!("### Запись {number}\n"));
    if !value.is_object() && !value.is_array() {
        out.put("**Значение:** ");
    }
    render(value, out, 0)?;
    Ok(true)
}

fn input(request: &Value) -> Result<Option<String>, &'static str> {
    let source = Path::new(request["path"].as_str().ok_or("JSON_SOURCE_UNAVAILABLE")?);
    if !source.is_absolute() {
        return Err("JSON_SOURCE_UNAVAILABLE");
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let input = options
        .open(source)
        .map_err(|_| "JSON_SOURCE_UNAVAILABLE")?;
    let metadata = input.metadata().map_err(|_| "JSON_SOURCE_UNAVAILABLE")?;
    if !metadata.is_file() {
        return Err("JSON_SOURCE_UNAVAILABLE");
    }
    if metadata.len() > MAX_SOURCE {
        return Err("JSON_SOURCE_LIMIT");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    input
        .take(MAX_SOURCE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "JSON_SOURCE_UNAVAILABLE")?;
    if bytes.len() as u64 > MAX_SOURCE {
        return Err("JSON_SOURCE_LIMIT");
    }
    Ok(plain_text::decode(&bytes))
}
pub(super) fn convert(request: &Value, text: &str) -> Result<Value, &'static str> {
    let limit = request["max_chars"]
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .filter(|v| *v >= 256 && *v <= 128 * 1024 * 1024)
        .ok_or("JSON_LIMIT_INVALID")?;
    if trim(&text).is_empty() {
        return Ok(failed("NO_SEARCHABLE_TEXT"));
    }
    let mut nonempty = text.lines().filter(|line| !trim(line).is_empty());
    let first = nonempty.next();
    let second = nonempty.next();
    let lines = request["json_lines"] == true
        || first
            .zip(second)
            .is_some_and(|(a, b)| parse(a).is_ok() && parse(b).is_ok());
    let mut out = Text {
        value: String::new(),
        chars: 0,
        limit,
        truncated: false,
    };
    let (mut records, mut legacy_numbers) = (0, false);
    if lines {
        for (line_number, line) in text.lines().enumerate() {
            if trim(line).is_empty() {
                continue;
            }
            if records >= MAX_RECORDS || out.full() {
                out.truncated = true;
                break;
            }
            let (value, legacy) = match parse(line) {
                Ok(value) => value,
                Err(error) => return Ok(parse_error(line, &error, Some(line_number + 1))),
            };
            legacy_numbers |= legacy;
            if record(&value, line_number + 1, &mut out)? {
                records += 1;
            }
        }
    } else {
        let (value, legacy) = match parse(&text) {
            Ok(value) => value,
            Err(error) => return Ok(parse_error(&text, &error, None)),
        };
        legacy_numbers = legacy;
        let values = match &value {
            Value::Array(values) => values.as_slice(),
            _ => std::slice::from_ref(&value),
        };
        for (index, value) in values.iter().enumerate() {
            if records >= MAX_RECORDS || out.full() {
                out.truncated = true;
                break;
            }
            if record(value, index + 1, &mut out)? {
                records += 1;
            }
        }
    }
    if trim(&out.value).is_empty() {
        return Ok(failed("NO_SEARCHABLE_TEXT"));
    }
    if legacy_numbers && out.chars + NUMBERS.chars().count() > limit {
        out.truncated = true;
    }
    let prefix = format!(
        "{}{}",
        if out.truncated { NOTE } else { "" },
        if legacy_numbers { NUMBERS } else { "" }
    );
    if !prefix.is_empty() {
        let remaining = limit.saturating_sub(prefix.chars().count());
        let body = out.value.chars().take(remaining).collect::<String>();
        out.value = prefix + &body;
    }
    Ok(
        json!({"status":"completed","records":records,"truncated":out.truncated,
        "legacy_numbers":legacy_numbers,"text":out.value,"extractor":"les.json.v2"}),
    )
}

pub(crate) fn read(request: &Value) -> Result<Value, &'static str> {
    if request.get("output").is_some() {
        return Err("JSON_READ_ONLY");
    }
    match input(request)? {
        Some(text) => convert(request, &text),
        None => Ok(json!({"status":"compatibility"})),
    }
}

pub(crate) fn run(
    request: &Value,
    bridge: impl Fn(u8, &Value) -> Result<Value, &'static str>,
) -> Result<Value, &'static str> {
    if request["schema"] != "les.json-source.v1" {
        return Err("JSON_READ_FAILED");
    }
    let text = match input(request)? {
        Some(text) => text,
        None => {
            let mut text = String::new();
            let mut offset = 0;
            loop {
                let value = bridge(1, &json!({"offset":offset,"limit":262144}))?;
                if let Some(error) = value.get("error") {
                    return Ok(failed(
                        error["code"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .unwrap_or("JSON_ENCODING_UNCERTAIN"),
                    ));
                }
                let part = value["text"].as_str().ok_or("JSON_ENCODING_UNCERTAIN")?;
                let count = part.chars().count();
                if value["complete"] != true && count != 262144 {
                    return Err("JSON_ENCODING_UNCERTAIN");
                }
                if text.len() + part.len() > 512 * 1024 * 1024 {
                    return Err("JSON_SOURCE_LIMIT");
                }
                text.push_str(part);
                offset += count;
                if value["complete"] == true {
                    break;
                }
            }
            text
        }
    };
    let mut result = convert(request, &text)?;
    if result["status"] != "completed" {
        return Ok(result);
    }
    let Value::String(text) = result
        .as_object_mut()
        .unwrap()
        .remove("text")
        .ok_or("JSON_READ_FAILED")?
    else {
        return Err("JSON_READ_FAILED");
    };
    let mut offset = 0;
    while offset < text.len() {
        let mut end = (offset + 512 * 1024).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let value = bridge(0, &json!({"text":&text[offset..end]}))?;
        if let Some(error) = value.get("error") {
            return Ok(failed(
                error["code"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("JSON_READ_FAILED"),
            ));
        }
        if value["ok"] != true {
            return Err("JSON_READ_FAILED");
        }
        offset = end;
    }
    result["characters"] = json!(text.chars().count());
    Ok(result)
}
