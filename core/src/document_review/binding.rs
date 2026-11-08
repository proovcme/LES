//! Request-bound review transitions shared by both transports. Source IO stays outside SQL locks.
use super::*;
fn chosen<'a>(state: &'a Value, args: &'a Value) -> &'a str {
    args["task_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| state["task_id"].as_str())
        .unwrap_or("")
}
pub(super) fn plan(conn: &Connection, r: &Value) -> Result<Value> {
    let state = &r["state"];
    let args = &r["arguments"];
    if args["operation"] == "start" {
        let selectors = args["selectors"]
            .as_array()
            .filter(|s| !s.is_empty() && s.len() <= 32)
            .ok_or_else(|| Failure::invalid("Выберите от 1 до 32 документов или разделов"))?;
        return Ok(
            json!({"selectors":selectors.iter().map(|s|source::selector(s,&state["allowed"],&state["files"])).collect::<Result<Vec<_>>>()?,"task_id":null}),
        );
    }
    let id = chosen(state, args);
    if args["operation"] == "status" && id.is_empty() {
        return Ok(json!({"selectors":[],"task_id":null}));
    }
    let task = task(conn, text(state, "owner")?, id)?;
    Ok(
        json!({"task_id":id,"selectors":task["units"].as_array().unwrap().iter().map(|u|u["selector"].clone()).collect::<Vec<_>>()}),
    )
}
pub(super) fn refresh(
    conn: &Connection,
    state: &mut Value,
    id: &str,
    snapshots: &Value,
    errors: &Value,
) -> Result<Value> {
    let fingerprints = snapshots
        .as_object()
        .ok_or_else(|| Failure::invalid("Некорректный снимок проверки"))?
        .iter()
        .map(|(k, v)| (k.clone(), v["fingerprint"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let mut current = status(conn, text(state, "owner")?, id, &json!(fingerprints))?;
    let count = errors.as_object().map_or(0, |o| o.len());
    current["source_errors"] = errors.clone();
    current["blocked_units"] = json!(count);
    current["needs_attention"] = json!(current["needs_attention"] == true || count > 0);
    current["next_unit_id"] = ["needs_recheck", "pending"]
        .into_iter()
        .find_map(|name| {
            current["units"]
                .as_array()
                .unwrap()
                .iter()
                .find(|u| {
                    u["state"] == name && snapshots.get(text(u, "unit_id").unwrap_or("")).is_some()
                })
                .map(|u| u["unit_id"].clone())
        })
        .unwrap_or(Value::Null);
    if state["task_id"] != id
        || !snapshots
            .as_object()
            .unwrap()
            .values()
            .any(|s| s["fingerprint"] == state["current_read"]["source_fingerprint"])
    {
        state["current_read"] = json!({});
        state["pending_read"] = Value::Null;
    }
    let mut compact = current.clone();
    compact.as_object_mut().unwrap().remove("units");
    compact.as_object_mut().unwrap().remove("source_errors");
    state["current_state"] = compact;
    Ok(current)
}
pub(super) fn context(state: &Value) -> Value {
    let current = &state["current_state"];
    let read = &state["current_read"];
    let unit = read.get("selector").and_then(|s| digest(s).ok());
    let next = if state["task_id"].as_str().is_none_or(|s| s.is_empty()) {
        json!({"operation":"start_or_select_review","instruction":"No review is selected. Use explicit selectors or an owned task_id from available_reviews."})
    } else if state["read_recorded"] != true && read["source_fingerprint"].is_string() {
        if let Some(cursor) = read.get("next_cursor").filter(|v| v.is_object()) {
            json!({"operation":"read","task_id":state["task_id"],"unit_id":unit,"fragment_index":cursor["fragment_index"],"offset":cursor["offset"],"instruction":"Continue this exact cursor; the unit is not fully delivered yet. Do not restart its inventory."})
        } else {
            json!({"operation":"record","task_id":state["task_id"],"unit_id":unit,"expected_revision":current["revision"],"source_fingerprint":read["source_fingerprint"],"model_must_supply":["status","note","evidence"],"instruction":"Choose evidence as {read_id,span_id} from current_read.quote_spans for the excerpt supporting your observation; source characters will be copied exactly. Literal {fragment_id,quote} is also allowed but must match exactly. This hint does not certify coverage or truth: record validates every range and quote. Do not call start again."})
        }
    } else if !current["next_unit_id"].is_null() {
        json!({"operation":"read","task_id":state["task_id"],"unit_id":current["next_unit_id"],"fragment_index":0,"offset":0,"instruction":"The inventory already exists. Read this pending/stale unit; start is not the next step."})
    } else {
        json!({"operation":"finish_or_status","instruction":"Use the current ledger counts. Blocked or needs_review states do not certify a correct or complete audit. No new start is needed."})
    };
    json!({"document_review":current,"current_read":read,"next_action":next,"instruction":"Follow next_action to progress this existing review; never repeat start after it returned a task_id. Resume only the review the user requests. The inventory covers selected indexed text units, not the whole project or visual pages. Conclusions are model-authored, never accepted by this tool. Never infer completion from chat history: only the current ledger counts establish saved progress. Use read cursors before record; needs_recheck requires a fresh read and new revision."})
}
pub(super) fn execute(conn: &Connection, r: &Value) -> Result<Value> {
    let mut state = r["state"].clone();
    let args = &r["arguments"];
    let op = text(args, "operation")?;
    let snapshots = &r["snapshots"];
    let errors = &r["errors"];
    let owner = text(&state, "owner")?.to_owned();
    state["pending_read"] = Value::Null;
    if op == "start" {
        let plan = plan(conn, r)?;
        let selectors = plan["selectors"].as_array().unwrap();
        if errors.as_object().is_some_and(|e| !e.is_empty())
            || selectors
                .iter()
                .any(|s| snapshots.get(&digest(s).unwrap_or_default()).is_none())
        {
            return Err(Failure::invalid(
                "Источник проверки недоступен; список не создан",
            ));
        }
        let id = create(
            conn,
            &json!({"owner":owner,"question":state["question"],"selectors":selectors}),
        )?;
        let id = id.as_str().unwrap();
        let payload = refresh(conn, &mut state, id, snapshots, errors)?;
        state["task_id"] = json!(id);
        return Ok(json!({"state":state,"payload":payload}));
    }
    let id = chosen(&state, args).to_owned();
    if op == "status" && id.is_empty() {
        let payload = operate(conn, "recent_reviews", &json!({"owner":owner}))?;
        return Ok(json!({"state":state,"payload":{"available_reviews":payload,"accepted":false}}));
    }
    let current = refresh(conn, &mut state, &id, snapshots, errors)?;
    state["task_id"] = json!(id);
    let mut chunk = Value::Null;
    let payload = match op {
        "status" => current,
        "history" => {
            let mut h = operate(
                conn,
                "history",
                &json!({"owner":owner,"task_id":id,"offset":args.get("offset").cloned().unwrap_or(json!(0))}),
            )?;
            h["task_id"] = json!(id);
            h["historical"] = json!(true);
            h["accepted"] = json!(false);
            h
        }
        "read" | "record" => {
            let read_unit = if op == "record" {
                state["current_read"]
                    .get("selector")
                    .map(digest)
                    .transpose()?
            } else {
                None
            };
            let unit=args["unit_id"].as_str().or(read_unit.as_deref()).or_else(||current["next_unit_id"].as_str()).ok_or_else(||Failure::invalid("Раздел недоступен или источник изменился; прочитайте status и обновите индекс"))?;
            let snap = snapshots.get(unit).ok_or_else(|| {
                Failure::invalid(
                    "Раздел недоступен или источник изменился; прочитайте status и обновите индекс",
                )
            })?;
            if op == "read" {
                let mut options = args.clone();
                options["max_chars"] = state["max_chars"].clone();
                let mut p = source::fragment(snap, &options)?;
                state["pending_read"] =
                    json!({"task_id":id,"unit_id":unit,"snapshot":snap,"payload":p});
                state["current_read"] = p.clone();
                state["read_recorded"] = json!(false);
                chunk = snap["fragments"]
                    .as_array()
                    .and_then(|f| f.iter().find(|f| f["id"] == p["fragment_id"]))
                    .cloned()
                    .unwrap_or(Value::Null);
                p["task_id"] = json!(id);
                p["revision"] = current["revision"].clone();
                p["unit_id"] = json!(unit);
                p
            } else {
                let mut input = args.clone();
                input["owner"] = json!(owner);
                input["task_id"] = json!(id);
                input["unit_id"] = json!(unit);
                input["snapshot"] = snap.clone();
                input["dependencies"] = args.get("dependencies").cloned().unwrap_or(json!([]));
                input["original_evidence"] = args["evidence"].clone();
                if args.get("request_id").is_none() {
                    input["request_id"] = json!(format!("auto:{}", record::request_hash(&input)?));
                }
                input["fingerprints"] = json!(snapshots
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v["fingerprint"].clone()))
                    .collect::<serde_json::Map<_, _>>());
                let revision = if let Some(revision) = record::replayed(conn, &input)? {
                    revision
                } else {
                    let (evidence, references) =
                        super::quotes::resolve(&state["current_read"], snap, &args["evidence"])?;
                    input["evidence"] = evidence;
                    input["evidence_references"] = references;
                    record::record(conn, &input)?
                };
                if digest(&state["current_read"]["selector"]).ok().as_deref() == Some(unit) {
                    state["read_recorded"] = json!(true);
                }
                let mut refreshed = refresh(conn, &mut state, &id, snapshots, errors)?;
                refreshed["saved_revision"] = revision;
                refreshed
            }
        }
        _ => return Err(Failure::invalid("Неизвестное действие проверки документов")),
    };
    Ok(json!({"state":state,"payload":payload,"chunk":chunk}))
}
pub(super) fn ack(conn: &Connection, r: &Value) -> Result<Value> {
    let mut state = r["state"].clone();
    let pending = &state["pending_read"];
    if pending.is_object() {
        let view = context(&state);
        let exact = encode(&view)?;
        let delivered = r["messages"].as_array().into_iter().flatten().any(|m| {
            let Some(content) = m["content"].as_str() else {
                return false;
            };
            if content.contains(&exact) {
                return true;
            }
            serde_json::from_str::<Value>(content).is_ok_and(|v| v["document_state"] == view)
        });
        if delivered {
            let p = &pending["payload"];
            record::remember(
                conn,
                &json!({"owner":state["owner"],"task_id":pending["task_id"],"unit_id":pending["unit_id"],"snapshot":pending["snapshot"],"fragment":p["fragment_id"],"start":p["offset"],"stop":p["stop"]}),
            )?;
            state["pending_read"] = Value::Null;
            state["delivery_revision"] = json!(state["delivery_revision"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(1));
        }
    }
    Ok(state)
}
