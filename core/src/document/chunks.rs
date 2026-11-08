//! Split at document boundaries with the selected embedding tokenizer's budget.
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

type Result<T> = std::result::Result<T, &'static str>;
pub(super) type Measure<'a> = dyn Fn(&str) -> Result<usize> + 'a;
const INVALID: &str = "DOCUMENT_CHUNKING_INVALID";
const MAX_CHUNKS: usize = 200_000;
static HEADINGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:#{1,6}\s+|(?:Пункт|Раздел|Статья|п\.|§)\s*\p{Nd}+(?:\.\p{Nd}+)+|\p{Nd}+(?:\.\p{Nd}+)+(?:\s+|\.|$))").unwrap()
});
static SENTENCE_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.!?][\s\x1c-\x1f]+").unwrap());
static MARKDOWN_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#+)[\s\x1c-\x1f](.*)").unwrap());

pub(super) fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn boundaries(text: &str, breaks: impl Iterator<Item = (usize, usize)>) -> Vec<&str> {
    let mut start = 0;
    let mut pieces = Vec::new();
    for (end, next) in breaks {
        let piece = trim(&text[start..end]);
        if !piece.is_empty() {
            pieces.push(piece);
        }
        start = next;
    }
    let tail = trim(&text[start..]);
    if !tail.is_empty() {
        pieces.push(tail);
    }
    pieces
}

fn clauses(text: &str) -> Vec<&str> {
    let mut start = 0;
    let breaks = text.split_inclusive('\n').filter_map(|line| {
        let at = start;
        start += line.len();
        HEADINGS.is_match(trim(line)).then_some((at, at))
    });
    boundaries(text, breaks)
}

fn sentences(text: &str) -> Vec<&str> {
    boundaries(
        text,
        SENTENCE_END
            .find_iter(text)
            .map(|m| (m.start() + 1, m.end())),
    )
}

pub(super) fn offsets(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect()
}

fn append(out: &mut Vec<String>, text: String) -> Result<()> {
    if out.len() >= MAX_CHUNKS {
        return Err("DOCUMENT_CHUNK_LIMIT");
    }
    out.push(text);
    Ok(())
}

struct Budget<'a> {
    size: usize,
    overlap: usize,
    chars_per_unit: usize,
    measure: &'a Measure<'a>,
}

impl Budget<'_> {
    fn overlap(&self, text: &str) -> Result<String> {
        if self.overlap == 0 {
            return Ok(String::new());
        }
        let mut keep = Vec::new();
        let mut length = 0;
        for sentence in sentences(text).into_iter().rev() {
            let cost = (self.measure)(sentence)?.saturating_add(1);
            if length + cost.min(self.overlap + 1) > self.overlap {
                if keep.is_empty() {
                    let start = sentence
                        .char_indices()
                        .rev()
                        .nth(self.overlap * self.chars_per_unit - 1)
                        .map_or(0, |(i, _)| i);
                    return Ok(trim(&sentence[start..]).to_owned());
                }
                break;
            }
            length += cost;
            keep.push(sentence);
        }
        keep.reverse();
        Ok(keep.join(" "))
    }

    fn pack(
        &self,
        pieces: Vec<&str>,
        separator: &str,
        split_clauses: bool,
        out: &mut Vec<String>,
    ) -> Result<()> {
        let mut current = String::new();
        let mut length = 0;
        for piece in pieces {
            let cost = (self.measure)(piece)?;
            if cost > self.size {
                if !current.is_empty() {
                    append(out, std::mem::take(&mut current))?;
                    length = 0;
                }
                if split_clauses {
                    self.pack(sentences(piece), " ", false, out)?;
                } else {
                    self.hard_slice(piece, out)?;
                }
                continue;
            }
            if !current.is_empty() && length + separator.len() + cost > self.size {
                let prefix = self.overlap(&current)?;
                append(out, std::mem::replace(&mut current, prefix))?;
                length = if current.is_empty() {
                    0
                } else {
                    (self.measure)(&current)?
                };
            }
            if !current.is_empty() {
                current.push_str(separator);
                length += separator.len();
            }
            current.push_str(piece);
            length += cost;
        }
        if !current.is_empty() {
            append(out, current)?;
        }
        Ok(())
    }

    fn hard_slice(&self, text: &str, out: &mut Vec<String>) -> Result<()> {
        let indices = offsets(text);
        let count = indices.len() - 1;
        let size = self.size * self.chars_per_unit;
        let overlap = (self.overlap * self.chars_per_unit).min(size / 4);
        let mut start = 0;
        while start < count {
            let end = (start + size).min(count);
            append(out, text[indices[start]..indices[end]].to_owned())?;
            start += size - overlap;
            if start + overlap >= count {
                if start < count {
                    append(out, text[indices[start]..].to_owned())?;
                }
                break;
            }
        }
        Ok(())
    }
}

fn page(text: &str, size: usize, overlap: usize) -> Result<Vec<String>> {
    let text = trim(text);
    let indices = offsets(text);
    let count = indices.len() - 1;
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < count {
        let mut end = (start + size).min(count);
        if end < count {
            let window = &text[indices[start]..indices[end]];
            let boundary = [window.rfind('\n'), window.rfind(". ")]
                .into_iter()
                .flatten()
                .max();
            if let Some(boundary) = boundary {
                let byte = indices[start] + boundary;
                let position = indices.binary_search(&byte).expect("character boundary");
                if position > start + size / 2 {
                    end = position + if text.as_bytes()[byte] == b'\n' { 1 } else { 2 };
                }
            }
        }
        let chunk = trim(&text[indices[start]..indices[end]]);
        if !chunk.is_empty() {
            append(&mut chunks, chunk.to_owned())?;
        }
        if end == count {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    Ok(chunks)
}

pub(crate) fn execute(request: &Value, measure: Option<&Measure<'_>>) -> Result<Value> {
    let text = request["text"].as_str().ok_or(INVALID)?;
    let size = request["size"]
        .as_u64()
        .filter(|n| (1..=1_000_000).contains(n))
        .ok_or(INVALID)? as usize;
    let overlap = request["overlap"]
        .as_u64()
        .filter(|n| *n < size as u64)
        .ok_or(INVALID)? as usize;
    let chunks = match request["kind"].as_str() {
        Some("page") if measure.is_none() => page(text, size, overlap)?,
        Some("structured") => {
            let budget = Budget {
                size,
                overlap,
                chars_per_unit: if measure.is_some() { 3 } else { 1 },
                measure: measure.unwrap_or(&|value: &str| Ok(value.chars().count())),
            };
            let mut chunks = Vec::new();
            budget.pack(clauses(text), "\n\n", true, &mut chunks)?;
            chunks
        }
        _ => return Err(INVALID),
    };
    Ok(json!({"chunks":chunks}))
}

pub(crate) fn markdown_sections(request: &Value) -> Result<Value> {
    let text = request["text"].as_str().ok_or(INVALID)?;
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut code = false;
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            code = !code;
        } else if let Some(heading) = (!code).then(|| MARKDOWN_HEADING.captures(line)).flatten() {
            let section = trim(&current);
            if !section.is_empty() {
                append(&mut chunks, section.to_owned())?;
            }
            current.clear();
            current.push_str(&heading[1]);
            current.push(' ');
            current.push_str(&heading[2]);
            current.push('\n');
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    let tail = trim(&current);
    if !tail.is_empty() {
        append(&mut chunks, tail.to_owned())?;
    }
    Ok(json!({"chunks":chunks}))
}
