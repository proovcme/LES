use super::*;
fn candidate(kind: &str, objects: Vec<Value>, required: bool) -> Value {
    json!({"kind":kind,"objects":objects,"required":required})
}
fn chronological(items: &[Value]) -> Vec<&Value> {
    let mut ordered: Vec<_> = items.iter().collect();
    ordered.sort_by_key(|item| item["context_sequence"].as_u64().unwrap_or(0));
    ordered
}
fn dialogue_groups(dialogue: &[Value]) -> Vec<Value> {
    let mut groups: Vec<Vec<Value>> = Vec::new();
    for message in dialogue {
        if message["role"] != "assistant" || groups.is_empty() {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(message.clone());
    }
    groups
        .into_iter()
        .enumerate()
        .map(|(sequence, group)| {
            let mut item = object(&format!("dialogue:{sequence}"), json!(group));
            item["context_sequence"] = json!(sequence);
            item
        })
        .collect()
}
fn pack(request: &Value, candidates: &[Value]) -> Result<Value> {
    let mut all = Vec::new();
    let mut rows = Vec::new();
    for candidate in candidates {
        let mut items = Vec::new();
        for item in list(candidate, "objects") {
            let text = object_render(item);
            let mut packed = json!({"id":item["object_id"],"render":text,"tokens":tokens(&text),"index":all.len()});
            all.push(item.clone());
            // A document is preferred whole. If it cannot fit this round,
            // admit complete typed reading blocks rather than discard every page.
            // Mandatory reads and provider exchanges remain indivisible.
            if candidate["required"] != true && item["payload"]["schema"] == "les.evidence-unit.v1"
            {
                let mut fallback = Vec::new();
                for (page, block) in list(&item["payload"], "blocks").iter().enumerate() {
                    let value = object(
                        &format!("{}:block:{page}", item["object_id"].as_str().ok_or(ERROR)?),
                        block.clone(),
                    );
                    let text = object_render(&value);
                    fallback.push(json!({"id":value["object_id"],"render":text,"tokens":tokens(&text),"index":all.len()}));
                    all.push(value);
                }
                packed["fallback"] = json!(fallback);
            }
            items.push(packed);
        }
        rows.push(
            json!({"kind":candidate["kind"],"required":candidate["required"],"objects":items}),
        );
    }
    let preset = &request["preset"];
    let result = crate::context::execute(
        &json!({"action":"pack","input_limit":preset["input_limit"],
        "optional_evidence_limit":request["optional_evidence_limit"],
        "generation_reserve":preset["generation_reserve"],"safety_reserve":preset["safety_reserve"],"candidates":rows}),
    )?;
    if result["overflow"] == true {
        return Ok(result);
    }
    let sections: Vec<_> = list(&result, "sections")
        .iter()
        .map(|section| {
            let objects: Result<Vec<_>> = list(section, "indexes")
                .iter()
                .map(|n| {
                    n.as_u64()
                        .and_then(|n| all.get(n as usize))
                        .cloned()
                        .ok_or(ERROR)
                })
                .collect();
            Ok(json!({"kind":section["kind"],"objects":objects?,"token_count":section["tokens"]}))
        })
        .collect::<Result<_>>()?;
    Ok(
        json!({"preset_id":preset["preset_id"],"input_budget_tokens":result["budget"],
        "generation_reserve_tokens":preset["generation_reserve"],"safety_reserve_tokens":preset["safety_reserve"],
        "included_tokens":result["used"],"sections":sections,"omissions":result["omissions"]}),
    )
}
pub(super) fn messages(packet: &Value, request_last: bool) -> Result<Value> {
    let mut sections = Vec::new();
    for section in list(packet, "sections") {
        let items = list(section, "objects");
        let text = items
            .iter()
            .map(object_render)
            .collect::<Vec<_>>()
            .join("\n");
        let mut row = json!({"kind":section["kind"],"render":text});
        if section["kind"] == "native_tool_exchange" {
            let mut messages = Vec::new();
            for item in chronological(items) {
                messages.extend(item["payload"].as_array().ok_or(ERROR)?.iter().cloned());
            }
            row["native"] = json!(messages);
        }
        if section["kind"] == "dialogue" {
            row["dialogue"]=json!(chronological(items).into_iter().flat_map(|item|item["payload"].as_array().map(Vec::as_slice).unwrap_or_else(||std::slice::from_ref(&item["payload"]))).map(|payload|{
                if payload.is_object() && ["user","assistant"].iter().any(|v|payload["role"]==*v) {
                    json!({"role":payload["role"],"content":if truth(&payload["content"]){string(&payload["content"])}else{String::new()}})
                } else {json!({"role":"user","content":render(payload)})}
            }).collect::<Vec<_>>());
        }
        sections.push(row);
    }
    Ok(crate::context::execute(
        &json!({"action":"messages","sections":sections,"request_last":request_last}),
    )?["messages"]
        .clone())
}
pub(super) fn assemble(request: &Value) -> Result<Value> {
    let sources = list(request, "source_map");
    let source_fields = [
        "index",
        "label",
        "doc_name",
        "source_page",
        "page",
        "url",
        "dataset_id",
        "qdrant_point_id",
        "parent_id",
        "section_heading",
        "parent_heading",
        "section_fragment_count",
        "context_origin",
    ];
    let projected: Vec<_> = sources
        .iter()
        .filter(|_| request["compact_source_evidence"] != true)
        .map(|source| {
            if let Some(object) = source.as_object() {
                Value::Object(
                    object
                        .iter()
                        .filter(|(key, _)| source_fields.contains(&key.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            } else {
                source.clone()
            }
        })
        .collect();
    let history = list(request, "native_tool_history");
    let mut previous = objects("prior-tool-turn", history);
    for (sequence, item) in previous.iter_mut().enumerate() {
        item["context_sequence"] = json!(sequence);
    }
    // Prefer recent history when optional groups cannot all fit; provider
    // messages are restored to chronological order after packing.
    previous.reverse();
    let mut current = objects("current-tool-turn", list(request, "native_tool_exchange"));
    for (sequence, item) in current.iter_mut().enumerate() {
        item["context_sequence"] = json!(history.len() + sequence);
    }
    let mut prior_dialogue = dialogue_groups(list(request, "dialogue"));
    let recent_dialogue: Vec<_> = prior_dialogue.pop().into_iter().collect();
    prior_dialogue.reverse();
    let mut candidates = vec![
        candidate(
            "profile_prefix",
            vec![object("profile:bound", request["profile_prefix"].clone())],
            true,
        ),
        candidate(
            "tool_shortlist",
            objects("tool", list(request, "shortlist")),
            false,
        ),
        candidate(
            "request",
            vec![object(
                "request:current",
                request["request_payload"].clone(),
            )],
            true,
        ),
        candidate("checkpoint", list(request, "checkpoint").to_vec(), false),
        candidate(
            "working_memory",
            list(request, "working_memory").to_vec(),
            false,
        ),
        candidate(
            "evidence",
            evidence_objects("evidence", list(request, "evidence")),
            false,
        ),
        candidate(
            "evidence",
            evidence_objects("current-document", list(request, "required_evidence")),
            true,
        ),
        candidate("native_tool_exchange", current, true),
        candidate("native_tool_exchange", previous, false),
        candidate("source_map", objects("source", &projected), false),
        candidate(
            "tool_exchange",
            objects("exchange", list(request, "tool_exchange")),
            false,
        ),
        candidate("dialogue", recent_dialogue, true),
        candidate("dialogue", prior_dialogue, false),
    ];
    if !sources.is_empty() && request["folder_question"] != true {
        // Admit mandatory turns and evidence before spending any space on its
        // description. Optional tool schemas precede evidence in the pack order.
        let proof_candidates = candidates
            .iter()
            .filter(|c| {
                c["required"] == true || c["kind"] == "tool_shortlist" || c["kind"] == "evidence"
            })
            .cloned()
            .collect::<Vec<_>>();
        let proof = pack(request, &proof_candidates)?;
        if proof["overflow"] == true {
            return Ok(proof);
        }
        let admitted = visible(&proof, sources);
        let spare = proof["input_budget_tokens"].as_i64().ok_or(ERROR)?
            - proof["included_tokens"].as_i64().ok_or(ERROR)?;
        if let Some(item) = coverage::for_window(sources, &admitted, spare) {
            candidates.push(candidate("request", vec![item], true));
        }
        for section in candidates.iter_mut().filter(|c| c["kind"] == "source_map") {
            section["objects"]
                .as_array_mut()
                .ok_or(ERROR)?
                .retain(|item| {
                    let id = &item["payload"]["index"];
                    !truth(id) || admitted.iter().any(|s| s["index"] == *id)
                });
        }
    }
    let packet = pack(request, &candidates)?;
    if packet["overflow"] == true {
        return Ok(packet);
    }
    Ok(json!({"messages":messages(&packet,true)?,"packet":packet}))
}
