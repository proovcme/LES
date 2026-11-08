//! Final sanitation and tokenizer budget, shared by all indexed document formats.
use super::chunks::{offsets, trim, Measure};
use regex::Regex;
use serde_json::{json, Value};
use std::{borrow::Cow, sync::LazyLock};

static DATA_URI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)data:[^\s\x1c-\x1f;,]{1,120}(?:;[^\s\x1c-\x1f,]{1,80})*;base64,[A-Za-zİıſK0-9+/=\s\x1c-\x1f]{128,}").unwrap()
});
static CONTROL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{3,}").unwrap());
static NEWLINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{4,}").unwrap());

pub(super) fn execute(
    request: &Value,
    measure: Option<&Measure<'_>>,
) -> Result<Value, &'static str> {
    let text = request["text"]
        .as_str()
        .ok_or("DOCUMENT_CHUNKING_INVALID")?;
    let budget = request["size"]
        .as_u64()
        .filter(|&n| n > 0)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("DOCUMENT_CHUNKING_INVALID")?;
    let minimum = request["minimum"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("DOCUMENT_CHUNKING_INVALID")?;

    let mut uris = 0;
    let mut controls = 0;
    let clean = if text
        .as_bytes()
        .windows(5)
        .any(|part| part.eq_ignore_ascii_case(b"data:"))
    {
        DATA_URI.replace_all(text, |_: &regex::Captures<'_>| {
            uris += 1;
            " [binary attachment removed] "
        })
    } else {
        Cow::Borrowed(text)
    };
    let (clean, binary) = remove_binary_runs(&clean);
    let clean = CONTROL.replace_all(&clean, |_: &regex::Captures<'_>| {
        controls += 1;
        " "
    });
    let clean = SPACES.replace_all(&clean, "  ");
    let clean = NEWLINES.replace_all(&clean, "\n\n\n");
    let clean = trim(&clean);
    let mut parts = Vec::new();
    let indices = offsets(clean);
    let count = indices.len() - 1;
    if count > 0 && count >= minimum {
        let mut start = 0;
        let mut ordinal = 0;
        while start < count {
            let remaining = &clean[indices[start]..];
            let cost = if let Some(measure) = measure {
                measure(remaining)?
            } else {
                count - start
            };
            let end = if cost <= budget {
                count
            } else {
                prefix(clean, &indices, start, budget, measure)?
            };
            let part = trim(&clean[indices[start]..indices[end]]);
            if !part.is_empty() {
                if part.chars().count() >= minimum {
                    let cost = length(part, measure)?;
                    if cost > budget {
                        return Err("DOCUMENT_EMBEDDING_BUDGET_EXCEEDED");
                    }
                    parts.push(json!({"text":part,"length":cost,"ord":ordinal}));
                }
                ordinal += 1;
            }
            let tail = &clean[indices[end]..];
            let next = indices[end] + tail.len() - trim(tail).len();
            start = indices.binary_search(&next).expect("character boundary");
        }
    }
    Ok(json!({"parts":parts,"quality":{
        "data_uri_removed":uris, "base64_runs_removed":binary,
        "control_chars_removed":controls, "sanitized":uris != 0 || binary != 0 || controls != 0
    }}))
}

fn remove_binary_runs(text: &str) -> (Cow<'_, str>, usize) {
    let mut output = String::new();
    let mut removed = 0;
    let mut previous = 0;
    let mut start = 0;
    for (end, byte) in text.bytes().chain([0]).enumerate() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=') {
            continue;
        }
        // A maximal ASCII run is bounded by a delimiter or a UTF-8 boundary.
        if end - start >= 256 {
            let run = &text[start..end];
            let body = run.trim_end_matches('=');
            if body.len() >= 256 && run.len() - body.len() <= 2 && !body.contains('=') {
                output.push_str(&text[previous..start]);
                output.push_str(" [binary payload removed] ");
                previous = end;
                removed += 1;
            }
        }
        start = end + 1;
    }
    if removed == 0 {
        (Cow::Borrowed(text), 0)
    } else {
        output.push_str(&text[previous..]);
        (Cow::Owned(output), removed)
    }
}

fn length(text: &str, measure: Option<&Measure<'_>>) -> Result<usize, &'static str> {
    measure.map_or_else(|| Ok(text.chars().count()), |measure| measure(text))
}

fn prefix(
    text: &str,
    indices: &[usize],
    start: usize,
    budget: usize,
    measure: Option<&Measure<'_>>,
) -> Result<usize, &'static str> {
    let best = if let Some(measure) = measure {
        let mut low = start + 1;
        let mut high = indices.len() - 1;
        let mut best = start;
        while low <= high {
            let middle = (low + high) / 2;
            if measure(&text[indices[start]..indices[middle]])? <= budget {
                best = middle;
                low = middle + 1;
            } else {
                high = middle - 1;
            }
        }
        best.max(start + 1)
    } else {
        start + budget
    };
    let floor = start + ((best - start) * 4 / 5).max(1);
    let boundary = text[indices[floor]..indices[best]]
        .rfind(['\n', ' '])
        .map_or(best, |index| {
            indices
                .binary_search(&(indices[floor] + index))
                .expect("character boundary")
        });
    Ok(boundary)
}
