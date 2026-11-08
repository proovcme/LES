//! Validate a bounded embedding batch before releasing any vectors.
use serde_json::{json, Value};

pub const LIMITS_HEADER: &str = "x-les-embed-limits";
pub const PROTOCOL: &str = "ollama-embed.v1";
const INVALID: &str = "UPSTREAM_EMBEDDING_RESPONSE_INVALID";
const TOO_LARGE: &str = "UPSTREAM_RESPONSE_TOO_LARGE";

pub struct Limits {
    pub(crate) count: usize,
    pub(crate) bytes: usize,
}

#[derive(Clone, Copy)]
pub enum Format {
    Ollama,
    OpenAi,
}

impl Limits {
    pub fn parse(header: &str) -> Option<Self> {
        let (count, bytes) = header.split_once('/')?;
        let count = count.parse().ok()?;
        let bytes = bytes.parse().ok()?;
        (count > 0 && (1..=1_048_576).contains(&bytes)).then_some(Self { count, bytes })
    }
}

pub async fn receive(
    mut response: reqwest::Response,
    limits: Limits,
    format: Format,
) -> Result<Vec<u8>, &'static str> {
    if response
        .content_length()
        .is_some_and(|length| length > limits.bytes as u64)
    {
        return Err(TOO_LARGE);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "UPSTREAM_STREAM_INTERRUPTED")?
    {
        if chunk.len() > limits.bytes - body.len() {
            return Err(TOO_LARGE);
        }
        body.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || encode(&body, limits.count, format))
        .await
        .map_err(|_| INVALID)?
}

pub(crate) fn encode(body: &[u8], count: usize, format: Format) -> Result<Vec<u8>, &'static str> {
    let payload: Value = serde_json::from_slice(body).map_err(|_| INVALID)?;
    let model = payload["model"]
        .as_str()
        .filter(|model| !model.trim().is_empty());
    let vectors = match format {
        Format::Ollama => {
            if model.is_none() {
                return Err(INVALID);
            }
            payload["embeddings"]
                .as_array()
                .ok_or(INVALID)?
                .iter()
                .collect::<Vec<_>>()
        }
        Format::OpenAi => {
            let rows = payload["data"].as_array().ok_or(INVALID)?;
            if rows.len() != count {
                return Err("UPSTREAM_EMBEDDING_COUNT_MISMATCH");
            }
            let mut sorted = vec![None; count];
            for row in rows {
                let index = row["index"]
                    .as_u64()
                    .or_else(|| row["index"].as_str()?.parse().ok())
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or(INVALID)?;
                let slot = sorted.get_mut(index).ok_or(INVALID)?;
                if slot.replace(&row["embedding"]).is_some() {
                    return Err(INVALID);
                }
            }
            sorted
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or(INVALID)?
        }
    };
    if vectors.len() != count {
        return Err("UPSTREAM_EMBEDDING_COUNT_MISMATCH");
    }
    let dimension = vectors
        .first()
        .and_then(|row| row.as_array())
        .map(Vec::len)
        .filter(|dimension| *dimension > 0)
        .ok_or(INVALID)?;
    let mut result = json!({
        "model_id":model,"count":count,"dimension":dimension,"usage":payload["usage"]
    })
    .to_string()
    .into_bytes();
    result.push(b'\n');
    for row in vectors {
        let row = row
            .as_array()
            .filter(|row| row.len() == dimension)
            .ok_or(INVALID)?;
        for value in row {
            let number = value
                .as_f64()
                .or_else(|| match format {
                    Format::OpenAi => value.as_str()?.trim().parse().ok(),
                    Format::Ollama => None,
                })
                .filter(|number| number.is_finite())
                .ok_or(INVALID)?;
            result.extend_from_slice(&number.to_le_bytes());
        }
    }
    Ok(result)
}
