use les_core::embedding_native::Embedder;
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{self, BufRead, Read, Write},
    path::Path,
};
pub fn encode(request: &Value) -> Result<Value, String> {
    let timeout = request
        .get("timeout_ms")
        .map_or(Some(120000), Value::as_u64)
        .filter(|v| (100..=600000).contains(v))
        .ok_or("INFERENCE_TIMEOUT_INVALID")?;
    let spec = std::env::var_os("LES_NATIVE_EMBEDDING_SPEC")
        .map(std::path::PathBuf::from)
        .ok_or("EMBEDDING_SPEC_MISSING")?;
    if !spec.is_absolute() {
        return Err("EMBEDDING_SPEC_INVALID".into());
    }
    let mut content = Vec::new();
    File::open(&spec)
        .map_err(|_| "EMBEDDING_SPEC_MISSING")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut content)
        .map_err(|_| "EMBEDDING_SPEC_INVALID")?;
    if content.len() > 1024 * 1024 {
        return Err("EMBEDDING_SPEC_INVALID".into());
    }
    let program = std::env::current_exe().map_err(|_| "EMBEDDING_NATIVE_WORKER_MISSING")?;
    let response = les_core::run_native_embeddings(&json!({"schema":"les.worker-host.v1",
        "program":program,"args":["--serve-embeddings",spec],"directory":spec.parent(),
        "timeout_ms":timeout,"context":{"spec_sha256":les_core::sha256_hex(&content)},"input":request}))?;
    if response["status"] != "completed" {
        // Static code stays an owned error field; never discard the child's diagnostic cause.
        eprintln!(
            "{}",
            json!({"event":"embedding_request_failed","code":response["code"],"cause":response["cause"]})
        );
        return Err(response["cause"]
            .as_str()
            .or_else(|| response["code"].as_str())
            .unwrap_or("EMBEDDING_WORKER_FAILED")
            .to_owned());
    }
    if response["result"]["status"] != "ok" {
        return Err(response["result"]["code"]
            .as_str()
            .unwrap_or("EMBEDDING_INFERENCE_FAILED")
            .to_owned());
    }
    Ok(response["result"]["result"].clone())
}
pub fn serve(spec: &Path) -> Result<(), &'static str> {
    let mut model = Embedder::load(spec)?;
    let mut input = io::stdin().lock();
    loop {
        let mut bytes = Vec::new();
        let n = (&mut input)
            .take(8 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "EMBEDDING_PIPE_FAILED")?;
        if n == 0 {
            return Ok(());
        }
        if n > 8 * 1024 * 1024 || !bytes.ends_with(b"\n") {
            return Err("EMBEDDING_INPUT_INVALID");
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "EMBEDDING_INPUT_INVALID")?;
        let compatible = value["input"]["model_id"]
            .as_str()
            .is_some_and(|id| model.matches(id, value["input"]["dimension"].as_u64()));
        let result = value["input"]["texts"]
            .as_array()
            .ok_or("EMBEDDING_INPUT_INVALID")
            .and_then(|rows| {
                rows.iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or("EMBEDDING_INPUT_INVALID")
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .and_then(|texts| {
                if compatible {
                    if !model.matches_connection(value["input"]["connection_revision_id"].as_str())
                    {
                        return Err("EMBEDDING_CONNECTION_REVISION_MISMATCH");
                    }
                    model.encode(&texts)
                } else {
                    Err("EMBEDDING_INDEX_MODEL_MISMATCH")
                }
            });
        let result = match result {
            Ok(v) => json!({"status":"ok","result":v}),
            Err(code) => json!({"status":"failed","code":code}),
        };
        println!(
            "{}",
            json!({"request_id":value["request_id"],"result":result})
        );
        io::stdout().flush().map_err(|_| "EMBEDDING_PIPE_FAILED")?;
    }
}
