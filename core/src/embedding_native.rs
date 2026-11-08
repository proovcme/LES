//! Offline ONNX embedding engine. Only verified local assets; no hub, downloads or Python.
use ort::{session::Session, value::Tensor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use tokenizers::Tokenizer;
type Result<T> = std::result::Result<T, &'static str>;
pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    model_id: String,
    output: String,
    pooling: String,
    dimension: usize,
    max_tokens: usize,
    connection_revision_id: Option<String>,
}
fn asset(value: &Value) -> Result<PathBuf> {
    let path = PathBuf::from(value["path"].as_str().ok_or("EMBEDDING_ASSET_INVALID")?);
    let hash = value["sha256"]
        .as_str()
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("EMBEDDING_ASSET_INVALID")?;
    if !path.is_absolute() {
        return Err("EMBEDDING_ASSET_INVALID");
    }
    let mut file = File::open(&path).map_err(|_| "EMBEDDING_ASSET_MISSING")?;
    let mut digest = Sha256::new();
    let mut block = [0u8; 128 * 1024];
    loop {
        let n = file
            .read(&mut block)
            .map_err(|_| "EMBEDDING_ASSET_MISSING")?;
        if n == 0 {
            break;
        }
        digest.update(&block[..n]);
    }
    if format!("{:x}", digest.finalize()) != hash.to_ascii_lowercase() {
        return Err("EMBEDDING_ASSET_CHANGED");
    }
    path.canonicalize().map_err(|_| "EMBEDDING_ASSET_MISSING")
}
impl Embedder {
    pub fn matches_connection(&self, revision: Option<&str>) -> bool {
        self.connection_revision_id.as_deref() == revision
    }
    pub fn matches(&self, model: &str, dimension: Option<u64>) -> bool {
        (model == self.model_id || (self.model_id == "BAAI/bge-m3" && model == "bge-m3"))
            && dimension.is_none_or(|v| v == self.dimension as u64)
    }
    pub fn load(spec_path: &Path) -> Result<Self> {
        if !spec_path.is_absolute() {
            return Err("EMBEDDING_SPEC_INVALID");
        }
        let mut bytes = Vec::new();
        File::open(spec_path)
            .map_err(|_| "EMBEDDING_SPEC_MISSING")?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "EMBEDDING_SPEC_INVALID")?;
        if bytes.len() > 1024 * 1024 {
            return Err("EMBEDDING_SPEC_INVALID");
        }
        let spec: Value = serde_json::from_slice(&bytes).map_err(|_| "EMBEDDING_SPEC_INVALID")?;
        if spec["schema"] != "les.embedding-worker.v1" {
            return Err("EMBEDDING_SPEC_INVALID");
        }
        let connection_revision_id = match spec.get("connection_revision_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(v))
                if !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control) =>
            {
                Some(v.clone())
            }
            _ => return Err("EMBEDDING_SPEC_INVALID"),
        };
        let model_id = spec["model_id"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or("EMBEDDING_SPEC_INVALID")?
            .to_owned();
        let output = spec["output"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or("EMBEDDING_SPEC_INVALID")?
            .to_owned();
        let pooling = spec["pooling"]
            .as_str()
            .filter(|v| matches!(*v, "cls" | "mean"))
            .ok_or("EMBEDDING_POOLING_INVALID")?
            .to_owned();
        let dimension = spec["dimension"]
            .as_u64()
            .filter(|v| (1..=65536).contains(v))
            .ok_or("EMBEDDING_SPEC_INVALID")? as usize;
        let max_tokens = spec["max_tokens"]
            .as_u64()
            .filter(|v| (2..=32768).contains(v))
            .ok_or("EMBEDDING_SPEC_INVALID")? as usize;
        let threads = spec["threads"]
            .as_u64()
            .filter(|v| (1..=16).contains(v))
            .ok_or("EMBEDDING_SPEC_INVALID")? as usize;
        let runtime = asset(&spec["runtime"])?;
        let model = asset(&spec["model"])?;
        let tokenizer = asset(&spec["tokenizer"])?;
        let external = spec["external_data"]
            .as_array()
            .filter(|v| v.len() <= 32)
            .ok_or("EMBEDDING_SPEC_INVALID")?;
        for item in external {
            if asset(item)?.parent() != model.parent() {
                return Err("EMBEDDING_ASSET_INVALID");
            }
        }
        let _ = ort::init_from(runtime)
            .map_err(|_| "EMBEDDING_RUNTIME_UNAVAILABLE")?
            .with_telemetry(false)
            .commit();
        let mut tokenizer =
            Tokenizer::from_file(tokenizer).map_err(|_| "EMBEDDING_TOKENIZER_INVALID")?;
        tokenizer
            .with_truncation(None)
            .map_err(|_| "EMBEDDING_TOKENIZER_INVALID")?;
        tokenizer.with_padding(None);
        let session = Session::builder()
            .map_err(|_| "EMBEDDING_RUNTIME_UNAVAILABLE")?
            .with_intra_threads(threads)
            .map_err(|_| "EMBEDDING_RUNTIME_UNAVAILABLE")?
            .commit_from_file(model)
            .map_err(|_| "EMBEDDING_MODEL_LOAD_FAILED")?;
        if !session.inputs().iter().any(|v| v.name() == "input_ids")
            || session
                .inputs()
                .iter()
                .any(|v| !matches!(v.name(), "input_ids" | "attention_mask" | "token_type_ids"))
            || !session.outputs().iter().any(|v| v.name() == output)
        {
            return Err("EMBEDDING_MODEL_SIGNATURE_INVALID");
        }
        Ok(Self {
            session,
            tokenizer,
            model_id,
            output,
            pooling,
            dimension,
            max_tokens,
            connection_revision_id,
        })
    }
    pub fn encode(&mut self, texts: &[String]) -> Result<Value> {
        if texts.is_empty()
            || texts.len() > 512
            || texts.len().saturating_mul(self.dimension) > 1_048_576
            || texts.iter().map(String::len).sum::<usize>() > 2 * 1024 * 1024
        {
            return Err("EMBEDDING_INPUT_INVALID");
        }
        let mut result = Vec::with_capacity(texts.len());
        for text in texts {
            let encoding = self
                .tokenizer
                .encode(text.as_str(), true)
                .map_err(|_| "EMBEDDING_TOKENIZER_INVALID")?;
            let tokens = encoding.len();
            if tokens == 0 || tokens > self.max_tokens {
                return Err("EMBEDDING_INPUT_TOO_LONG");
            }
            let mut inputs = Vec::new();
            for input in self.session.inputs() {
                let data = match input.name() {
                    "input_ids" => encoding
                        .get_ids()
                        .iter()
                        .map(|v| *v as i64)
                        .collect::<Vec<_>>(),
                    "attention_mask" => encoding
                        .get_attention_mask()
                        .iter()
                        .map(|v| *v as i64)
                        .collect(),
                    "token_type_ids" => encoding.get_type_ids().iter().map(|v| *v as i64).collect(),
                    _ => return Err("EMBEDDING_MODEL_SIGNATURE_INVALID"),
                };
                let tensor = Tensor::from_array(([1usize, tokens], data.into_boxed_slice()))
                    .map_err(|_| "EMBEDDING_INPUT_INVALID")?;
                inputs.push((input.name().to_owned(), tensor));
            }
            let outputs = self
                .session
                .run(inputs)
                .map_err(|_| "EMBEDDING_INFERENCE_FAILED")?;
            let (shape, values) = outputs
                .get(&self.output)
                .ok_or("EMBEDDING_OUTPUT_INVALID")?
                .try_extract_tensor::<f32>()
                .map_err(|_| "EMBEDDING_OUTPUT_INVALID")?;
            if shape.len() != 3
                || shape[0] != 1
                || shape[1] != tokens as i64
                || shape[2] != self.dimension as i64
                || values.len() != tokens * self.dimension
            {
                return Err("EMBEDDING_OUTPUT_INVALID");
            }
            let mut vector = if self.pooling == "cls" {
                values[..self.dimension].to_vec()
            } else {
                let mut vector = vec![0.0f32; self.dimension];
                for token in values.chunks_exact(self.dimension) {
                    for (sum, value) in vector.iter_mut().zip(token) {
                        *sum += *value / tokens as f32;
                    }
                }
                vector
            };
            let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            if !norm.is_finite() || norm < 1e-12 || vector.iter().any(|v| !v.is_finite()) {
                return Err("EMBEDDING_OUTPUT_INVALID");
            }
            for value in &mut vector {
                *value /= norm;
            }
            result.push(vector.into_iter().map(f64::from).collect::<Vec<_>>());
        }
        Ok(
            json!({"model_id":self.model_id,"dimension":self.dimension,"runtime":"onnx_cpu","vectors":result}),
        )
    }
}
