//! Bounded probes send a fixed sample, never conversation or document content.
use super::*;
use futures_util::StreamExt;

const CAPABILITIES: &[&str] = &[
    "models",
    "chat_completions",
    "streaming",
    "tools",
    "structured_output",
    "responses",
    "embeddings",
    "token_count",
    "rerank",
];
const LIMIT: usize = 262_144;
const PROBE: &str = "LES capability check.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Manual,
    LocalEmbeddings,
}

async fn exchange(
    endpoint: &crate::model_runtime::Endpoint,
    auth: &Option<String>,
    path: &str,
    body: Option<Value>,
    native: bool,
) -> Result<(u16, Vec<u8>), &'static str> {
    let mut url = endpoint.url.clone();
    let base = url.path().trim_end_matches('/');
    let path = if native {
        let base = base
            .strip_suffix("/api/v1")
            .or_else(|| base.strip_suffix("/v1"))
            .unwrap_or(base);
        format!("{base}/api/{path}")
    } else if base.ends_with("/v1") {
        format!("{base}/{path}")
    } else {
        format!("{base}/v1/{path}")
    };
    url.set_path(&path);
    exchange_url(endpoint, auth, url, body, Duration::from_secs(25)).await
}

async fn exchange_url(
    endpoint: &crate::model_runtime::Endpoint,
    auth: &Option<String>,
    url: reqwest::Url,
    body: Option<Value>,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), &'static str> {
    tokio::time::timeout(timeout, async {
        let reply = match body {
            Some(body) => {
                crate::model_http::open(
                    url,
                    endpoint.addresses.clone(),
                    auth.clone(),
                    body.to_string().into_bytes(),
                    timeout,
                    false,
                )
                .await?
            }
            None => {
                crate::model_http::get_authorized(
                    url,
                    endpoint.addresses.clone(),
                    auth.clone(),
                    timeout,
                )
                .await?
            }
        };
        let mut bytes = Vec::new();
        let mut stream = reply.source;
        while let Some(piece) = stream.next().await {
            let piece = piece?;
            if bytes.len().saturating_add(piece.len()) > LIMIT {
                return Err("PROBE_BODY_LIMIT");
            }
            bytes.extend_from_slice(&piece);
        }
        Ok((reply.status.as_u16(), bytes))
    })
    .await
    .map_err(|_| "UPSTREAM_TIMEOUT")?
}

fn request(capability: &str, model: &Value) -> (&'static str, Option<Value>) {
    let messages = json!([{"role":"user","content":PROBE}]);
    let mut body = json!({"model":model,"messages":messages,"max_tokens":16,"temperature":0});
    match capability {
        "models" => ("models", None),
        "responses" => (
            "responses",
            Some(json!({"model":model,"input":PROBE,"max_output_tokens":16})),
        ),
        "embeddings" => ("embeddings", Some(json!({"model":model,"input":[PROBE]}))),
        "token_count" => (
            "messages/count_tokens",
            Some(json!({"model":model,"messages":messages})),
        ),
        "rerank" => (
            "rerank",
            Some(json!({"model":model,"query":PROBE,"documents":[PROBE]})),
        ),
        _ => {
            if capability == "streaming" {
                body["stream"] = json!(true);
            }
            if capability == "tools" {
                body["messages"][0]["content"] =
                    json!("Call the les_capability_probe function once.");
                body["max_tokens"] = json!(128);
                body["tools"] = json!([{"type":"function","function":{"name":"les_capability_probe",
                    "description":"Verify tool calling","parameters":{"type":"object","properties":{}}}}]);
            }
            if capability == "structured_output" {
                body["messages"][0]["content"] = json!("Return the JSON object {\"ok\":true}.");
                body["max_tokens"] = json!(128);
                body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"les_capability_probe",
                    "strict":true,"schema":{"type":"object","properties":{"ok":{"type":"boolean"}},
                    "required":["ok"],"additionalProperties":false}}});
            }
            ("chat/completions", Some(body))
        }
    }
}
fn vector(value: &Value) -> bool {
    value.as_array().is_some_and(|v| {
        !v.is_empty()
            && v.iter().all(|n| n.as_f64().is_some_and(f64::is_finite))
            && v.iter().any(|n| n.as_f64().is_some_and(|v| v != 0.0))
    })
}

fn freetoken_catalogue_context(data: &[u8], model: &Value) -> Option<u64> {
    let catalogue: Value = serde_json::from_slice(data).ok()?;
    let card = catalogue["data"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"] == *model)?;
    ["context_length", "max_model_len"]
        .into_iter()
        .filter_map(|key| card[key].as_u64().filter(|n| *n > 0))
        .min()
}

fn freetoken_context(catalogue: Option<u64>, stats: &Value, model: &Value) -> Option<u64> {
    if stats["model"]["id"].as_str().is_none() || stats["model"]["id"] != *model {
        return catalogue;
    }
    let reported = stats["model"]["ctx"].as_u64().filter(|n| *n > 0);
    let kv = stats["kv"]["total_pages"]
        .as_u64()
        .zip(stats["kv"]["page_size"].as_u64())
        .and_then(|(pages, size)| pages.checked_mul(size))
        .filter(|n| *n > 0);
    catalogue.into_iter().chain(reported).chain(kv).min()
}

pub(super) async fn llama_cpp_props(
    endpoint: &crate::model_runtime::Endpoint,
    auth: &Option<String>,
    model: Option<&str>,
) -> Option<Value> {
    let mut url = endpoint.url.clone();
    let base = url.path().trim_end_matches('/');
    let path = format!("{}/props", base.strip_suffix("/v1").unwrap_or(base));
    url.set_path(&path);
    if let Some(model) = model {
        url.query_pairs_mut().append_pair("model", model);
    }
    let (200, raw) = exchange_url(endpoint, auth, url, None, Duration::from_secs(2))
        .await
        .ok()?
    else {
        return None;
    };
    let props: Value = serde_json::from_slice(&raw).ok()?;
    (props["default_generation_settings"]["n_ctx"].is_u64()
        && (props["model_alias"].is_string() || props["model_path"].is_string()))
    .then_some(props)
}

fn llama_cpp_context(props: &Value, model: &Value) -> Option<u64> {
    if props["model_alias"] != *model && props["model_path"] != *model {
        return None;
    }
    props["default_generation_settings"]["n_ctx"]
        .as_u64()
        .filter(|n| *n > 0)
}

fn verified(capability: &str, data: &[u8]) -> bool {
    if capability == "streaming" {
        let Ok(body) = std::str::from_utf8(data) else {
            return false;
        };
        let mut choices = false;
        for line in body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
        {
            if line == "[DONE]" {
                return choices;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                return false;
            };
            if value["error"].is_object() {
                return false;
            }
            choices |= value["choices"].as_array().is_some_and(|v| !v.is_empty());
        }
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return false;
    };
    if value["error"].is_object() {
        return false;
    }
    let message = &value["choices"][0]["message"];
    match capability {
        "models" => value["data"].is_array(),
        "chat_completions" => {
            message["content"].is_string()
                || message["reasoning_content"].is_string()
                || message["reasoning"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty())
        }
        "tools" => message["tool_calls"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|v| v["function"]["name"] == "les_capability_probe")
        }),
        "structured_output" => message["content"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Value>(v).ok())
            .is_some_and(|v| v["ok"].is_boolean()),
        "responses" => value["output"].is_array(),
        "embeddings" => value["data"]
            .as_array()
            .is_some_and(|v| v.len() == 1 && vector(&v[0]["embedding"])),
        "token_count" => value["input_tokens"].as_u64().is_some_and(|v| v > 0),
        "rerank" => {
            value["results"][0]["index"] == 0
                && value["results"][0]["relevance_score"]
                    .as_f64()
                    .is_some_and(f64::is_finite)
        }
        _ => false,
    }
}

pub(crate) async fn run(
    context: Arc<Context>,
    id: &str,
    actor: &str,
    input: &Value,
    mode: Mode,
) -> Result<Value, &'static str> {
    let fields = input
        .as_object()
        .filter(|v| v.len() == 2)
        .ok_or("INVALID_MODEL_SETTINGS")?;
    let revision = fields
        .get("revision_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or("INVALID_MODEL_SETTINGS")?
        .to_owned();
    let selected = fields
        .get("capabilities")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= CAPABILITIES.len())
        .ok_or("INVALID_MODEL_SETTINGS")?;
    if selected
        .iter()
        .any(|v| v.as_str().is_none_or(|s| !CAPABILITIES.contains(&s)))
    {
        return Err("INVALID_MODEL_SETTINGS");
    }
    let _permit = context
        .workspace_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "MODEL_SETTINGS_UNAVAILABLE")?;
    let owner = context.clone();
    let id = id.to_owned();
    let lookup = revision.clone();
    let (row, endpoint, auth) = tokio::task::spawn_blocking(move || {
        let r = crate::library::catalog(
            &owner.database,
            "model_registry_revision",
            json!({"revision_id":lookup}),
        );
        let row = r["value"].clone();
        if row["connection_id"] != id {
            return Err("CONNECTION_REVISION_NOT_FOUND");
        }
        let head = crate::library::catalog(
            &owner.database,
            "model_registry_get",
            json!({"connection_id":id}),
        );
        if row["enabled"] != 1 || head["value"]["enabled"] != 1 {
            return Err("CONNECTION_DISABLED");
        }
        let locality = row["locality"].as_str().ok_or("ENDPOINT_INVALID")?;
        if mode != Mode::Manual
            && (locality != "loopback" || head["value"]["revision_id"] != row["revision_id"])
        {
            return Err("INDEX_SETUP_MODEL_CHANGED");
        }
        let endpoint = crate::model_runtime::validate(
            row["base_url"].as_str().ok_or("ENDPOINT_INVALID")?,
            locality,
            true,
        )
        .map_err(|_| "ENDPOINT_INVALID")?;
        let port = endpoint.url.port_or_known_default();
        if locality == "loopback" && (port == Some(owner.listen_port)) {
            return Err("ENDPOINT_INVALID");
        }
        let env = std::env::var_os("LES_ENV_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| owner.state_root.join(".env"));
        let key = les_core::model_secrets::resolve(&env, row["secret_ref"].as_str(), |key| {
            std::env::var(key).ok()
        })?;
        Ok((row, endpoint, key.map(|v| format!("Bearer {v}"))))
    })
    .await
    .map_err(|_| "MODEL_SETTINGS_UNAVAILABLE")??;
    let models_only = selected.iter().all(|v| v == "models");
    if !models_only
        && endpoint.locality == "loopback"
        && les_core::system_memory::available().unwrap_or(0) < 4 * 1024 * 1024 * 1024
    {
        return Err("MODEL_MEMORY_GUARD");
    }
    let model = &row["model_id"];
    let freetoken = row["extension_type"] == "freetoken" && endpoint.locality == "loopback";
    let mut catalogue_context = None;
    let mut observations = Vec::new();
    let mut options = json!({"max_output_field":"max_tokens"});
    let supported = |observations: &[Value], name: &str| {
        observations
            .iter()
            .any(|v| v["capability"] == name && v["state"] == "supported")
    };
    for cap in CAPABILITIES.iter().copied() {
        let (state, evidence, detail) = if selected.iter().any(|v| v == cap) {
            let (path, mut body) = request(cap, model);
            if path == "chat/completions" && options["reasoning_control"] == "reasoning_effort_none"
            {
                if let Some(body) = &mut body {
                    body["reasoning_effort"] = json!("none");
                }
            }
            match exchange(&endpoint, &auth, path, body, false).await {
                Ok((status, bytes)) if (200..300).contains(&status) && verified(cap, &bytes) => {
                    if cap == "chat_completions" && endpoint.locality == "loopback" {
                        let response: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                        let message = &response["choices"][0]["message"];
                        let has_reasoning = ["reasoning", "reasoning_content"]
                            .iter()
                            .any(|key| message[*key].as_str().is_some_and(|s| !s.is_empty()));
                        if has_reasoning || response["system_fingerprint"] == "fp_ollama" {
                            let sample = json!({"model":model,"messages":[{"role":"user","content":"Reply with OK."}],
                                "temperature":0,"max_tokens":32,"reasoning_effort":"none"});
                            if let Ok((200, raw)) =
                                exchange(&endpoint, &auth, path, Some(sample), false).await
                            {
                                if serde_json::from_slice::<Value>(&raw).is_ok_and(|v| {
                                    let choice = &v["choices"][0];
                                    let msg = &choice["message"];
                                    choice["finish_reason"] == "stop"
                                        && msg["content"].as_str().is_some_and(|s| {
                                            !s.trim().is_empty() && !s.contains("<think>")
                                        })
                                        && ["reasoning", "reasoning_content"]
                                            .iter()
                                            .all(|key| msg[*key].as_str().unwrap_or("").is_empty())
                                }) {
                                    options["reasoning_control"] = json!("reasoning_effort_none");
                                }
                            }
                        }
                    }
                    if freetoken && cap == "models" {
                        catalogue_context = freetoken_catalogue_context(&bytes, model);
                    }
                    ("supported", "probe", format!("http_{status}"))
                }
                Ok((404 | 405 | 501, _)) => ("unsupported", "probe", "endpoint_unavailable".into()),
                Ok((status, _)) => ("unknown", "probe", format!("http_{status}_unverified")),
                Err(code) => ("unknown", "probe", code.into()),
            }
        } else {
            ("unknown", "unavailable", "not_requested".into())
        };
        observations.push(
            json!({"capability":cap,"state":state,"evidence_source":evidence,"detail":detail}),
        );
    }
    if freetoken {
        let stats = if supported(&observations, "chat_completions") {
            match exchange(&endpoint, &auth, "stats", None, false).await {
                Ok((200, raw)) => serde_json::from_slice(&raw).unwrap_or(Value::Null),
                _ => Value::Null,
            }
        } else {
            Value::Null
        };
        if let Some(tokens) = freetoken_context(catalogue_context, &stats, model) {
            options["observed_context_tokens"] = json!(tokens.to_string());
        }
    }
    if row["extension_type"] == "llama_cpp"
        && endpoint.locality == "loopback"
        && supported(&observations, "chat_completions")
    {
        if let Some(tokens) = llama_cpp_props(&endpoint, &auth, model.as_str())
            .await
            .and_then(|props| llama_cpp_context(&props, model))
        {
            options["observed_context_tokens"] = json!(tokens.to_string());
        }
    }
    if row["extension_type"] == "ollama" && endpoint.locality != "remote" {
        let context_tokens = row["requested_context_tokens"].as_u64().unwrap_or(8192);
        if supported(&observations, "chat_completions") {
            let mut chat_options = json!({"num_predict":1});
            if let Some(context) = row["requested_context_tokens"].as_u64() {
                chat_options["num_ctx"] = json!(context);
            }
            let payload = json!({"model":model,"messages":[{"role":"user","content":PROBE}],"think":false,"stream":false,
                "options":chat_options,"keep_alive":"30s"});
            if let Ok((200, raw)) = exchange(&endpoint, &auth, "chat", Some(payload), true).await {
                if serde_json::from_slice::<Value>(&raw)
                    .is_ok_and(|v| v["message"].is_object() && v["done"] == true)
                {
                    options["chat_protocol"] = json!("native_chat_v1");
                    if let (Some(name), Ok((200, raw))) = (
                        model.as_str(),
                        exchange(&endpoint, &auth, "ps", None, true).await,
                    ) {
                        let tagged = (!name.contains(':')).then(|| format!("{name}:latest"));
                        if let Ok(running) = serde_json::from_slice::<Value>(&raw) {
                            if let Some(active) = running["models"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .find(|entry| {
                                    ["name", "model"].iter().any(|key| {
                                        entry[*key].as_str().is_some_and(|value| {
                                            value == name || Some(value) == tagged.as_deref()
                                        })
                                    })
                                })
                                .and_then(|entry| entry["context_length"].as_u64())
                                .filter(|n| *n > 0)
                            {
                                options["active_context_tokens"] = json!(active.to_string());
                            }
                        }
                    }
                    let _ = exchange(
                        &endpoint,
                        &auth,
                        "generate",
                        Some(json!({"model":model,"keep_alive":0})),
                        true,
                    )
                    .await;
                    // Verify structured output through the transport used by chat.
                    // A compatibility response may spend its short limit thinking.
                    if selected.iter().any(|v| v == "structured_output") {
                        let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false});
                        let mut body = json!({"model":model,"messages":[{"role":"user","content":"Return JSON with ok=true."}],"think":false,"stream":false,"format":schema,"options":chat_options});
                        body["options"]["num_predict"] = json!(128);
                        let ok = match exchange(&endpoint, &auth, "chat", Some(body), true).await {
                            Ok((200, raw)) => serde_json::from_slice::<Value>(&raw)
                                .ok()
                                .filter(|v| v["done"] == true && v["done_reason"] != "length")
                                .and_then(|v| {
                                    serde_json::from_str::<Value>(v["message"]["content"].as_str()?)
                                        .ok()
                                })
                                .is_some_and(|v| {
                                    v.as_object().is_some_and(|o| o.len() == 1)
                                        && v["ok"].is_boolean()
                                }),
                            _ => false,
                        };
                        if let Some(observation) = observations
                            .iter_mut()
                            .find(|v| v["capability"] == "structured_output")
                        {
                            observation["state"] = json!(if ok { "supported" } else { "unknown" });
                            observation["detail"] = json!(if ok {
                                "native_json_schema_verified"
                            } else {
                                "native_json_schema_unverified"
                            });
                        }
                    }
                    if let Ok((200, raw)) =
                        exchange(&endpoint, &auth, "show", Some(json!({"model":model})), true).await
                    {
                        if let Some(n) = serde_json::from_slice::<Value>(&raw).ok().and_then(|v| {
                            v["model_info"].as_object().and_then(|map| {
                                map.iter()
                                    .filter(|(key, _)| key.ends_with(".context_length"))
                                    .filter_map(|(_, v)| v.as_u64())
                                    .filter(|n| *n > 0)
                                    .max()
                            })
                        }) {
                            options["observed_context_tokens"] = json!(n.to_string());
                        }
                    }
                }
            }
        }
        if supported(&observations, "embeddings") {
            let payload = json!({"model":model,"input":[PROBE],"truncate":false,"keep_alive":0,"options":{"num_ctx":context_tokens}});
            if let Ok((200, raw)) = exchange(&endpoint, &auth, "embed", Some(payload), true).await {
                if serde_json::from_slice::<Value>(&raw)
                    .is_ok_and(|v| v["model"] == *model && vector(&v["embeddings"][0]))
                {
                    options["embedding_protocol"] = json!("native_embedding_v1");
                }
            }
        }
    }
    let actor = actor.to_owned();
    if mode != Mode::Manual {
        let not_ready = "EMBEDDING_MODEL_NOT_READY";
        let evidence = observations
            .iter()
            .find(|row| row["capability"] == "embeddings")
            .ok_or(not_ready)?;
        if evidence["state"] != "supported" {
            return Err(match evidence["detail"].as_str() {
                Some(
                    "MODEL_ENDPOINT_UNREACHABLE" | "UPSTREAM_UNREACHABLE" | "UPSTREAM_TIMEOUT",
                ) => "MODEL_ENDPOINT_UNREACHABLE",
                _ => not_ready,
            });
        }
    }
    let snapshot = tokio::task::spawn_blocking(move || {
        les_core::catalog::record_probe(&context.database, &revision, &actor, observations, options)
    })
    .await
    .map_err(|_| "MODEL_SETTINGS_UNAVAILABLE")?
    .map_err(|_| "MODEL_SETTINGS_UNAVAILABLE")?;
    let capabilities = snapshot["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            json!({"name":row["capability"],"state":row["state"],"detail":row["detail"],
            "observed_at":row["observed_at"],"evidence_source":row["evidence_source"]})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"revision_id":snapshot["connection_revision_id"],"snapshot_id":snapshot["snapshot_id"],
        "observed_at":snapshot["observed_at"],"expires_at":snapshot["expires_at"],"capabilities":capabilities}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_probe_accepts_nonempty_mlx_reasoning() {
        let reply = |reasoning: &str| {
            json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":null,"reasoning":reasoning}}]})
                .to_string()
        };
        assert!(verified("chat_completions", reply("Checking.").as_bytes()));
        assert!(!verified("chat_completions", reply(" ").as_bytes()));
    }

    #[test]
    fn freetoken_context_respects_the_served_model_and_allocated_pool() {
        let model = json!("served-model");
        let catalogue = br#"{"data":[{"id":"other","context_length":1024},{"id":"served-model","context_length":131072,"max_model_len":65536}]}"#;
        let ceiling = freetoken_catalogue_context(catalogue, &model);
        assert_eq!(ceiling, Some(65536));
        let stats = json!({"model":{"id":"served-model","ctx":131072},"kv":{"total_pages":256,"used_pages":230,"page_size":64}});
        assert_eq!(freetoken_context(ceiling, &stats, &model), Some(16384));
        let mut outdated = stats;
        outdated["model"]["id"] = json!("other");
        assert_eq!(freetoken_context(ceiling, &outdated, &model), ceiling);
        assert_eq!(freetoken_context(ceiling, &Value::Null, &model), ceiling);
    }

    #[test]
    fn llama_cpp_context_uses_the_selected_slot_window() {
        let model = json!("served-model");
        let props = json!({"model_alias":"served-model","default_generation_settings":{"n_ctx":4096},"total_slots":4});
        assert_eq!(llama_cpp_context(&props, &model), Some(4096));
        assert_eq!(llama_cpp_context(&props, &json!("other")), None);
        let mut router = props;
        router["default_generation_settings"]["n_ctx"] = json!(0);
        assert_eq!(llama_cpp_context(&router, &model), None);
    }
}
