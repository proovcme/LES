use super::*;
fn candidate(kind: &str, objects: Vec<Value>, required: bool) -> Value {
    json!({"kind":kind,"objects":objects,"required":required})
}
fn pack(request: &Value, candidates: &[Value]) -> Result<Value> {
    let mut all = Vec::new();
    let mut rows = Vec::new();
    for candidate in candidates {
        let mut items = Vec::new();
        for item in list(candidate, "objects") {
            let text = object_render(item);
            items.push(json!({"id":item["object_id"],"render":text,"tokens":tokens(&text),"index":all.len()}));
            all.push(item.clone());
        }
        rows.push(
            json!({"kind":candidate["kind"],"required":candidate["required"],"objects":items}),
        );
    }
    let preset = &request["preset"];
    let result = crate::context::execute(
        &json!({"action":"pack","input_limit":preset["input_limit"],
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
            for item in items {
                messages.extend(item["payload"].as_array().ok_or(ERROR)?.iter().cloned());
            }
            row["native"] = json!(messages);
        }
        if section["kind"] == "dialogue" {
            row["dialogue"]=json!(items.iter().map(|item|{
                let payload=&item["payload"];
                if payload.is_object() && ["user","assistant"].iter().any(|v|payload["role"]==*v) {
                    json!({"role":payload["role"],"content":if truth(&payload["content"]){string(&payload["content"])}else{String::new()}})
                } else {json!({"role":"user","content":object_render(item)})}
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
        "section_fragment_count",
        "context_origin",
    ];
    let projected: Vec<_> = sources
        .iter()
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
            objects("evidence", list(request, "evidence")),
            false,
        ),
        candidate(
            "evidence",
            objects("current-document", list(request, "required_evidence")),
            true,
        ),
        candidate(
            "native_tool_exchange",
            objects("current-tool-turn", list(request, "native_tool_exchange")),
            true,
        ),
        candidate("source_map", objects("source", &projected), false),
        candidate(
            "tool_exchange",
            objects("exchange", list(request, "tool_exchange")),
            false,
        ),
        candidate(
            "dialogue",
            objects("dialogue", list(request, "dialogue")),
            !list(request, "dialogue").is_empty(),
        ),
    ];
    let reservation = coverage::reservation(sources);
    if let Some(item) = &reservation {
        candidates.push(candidate("request", vec![item.clone()], true));
    }
    let packet = pack(request, &candidates)?;
    if packet["overflow"] == true {
        return Ok(packet);
    }
    let indexes: Vec<_> = visible(&packet, sources)
        .iter()
        .filter(|s| s.is_object())
        .map(|s| s["index"].clone())
        .collect();
    for candidate in candidates.iter_mut().filter(|c| c["kind"] == "source_map") {
        candidate["objects"]
            .as_array_mut()
            .ok_or(ERROR)?
            .retain(|item| {
                let payload = &item["payload"];
                !payload.is_object()
                    || !truth(&payload["index"])
                    || indexes.contains(&payload["index"])
            });
    }
    let mut packet = pack(request, &candidates)?;
    if packet["overflow"] == true {
        return Ok(packet);
    }
    if reservation.is_some() {
        let visible = visible(&packet, sources);
        packet = coverage::finalize(packet, sources, &visible)?;
    }
    Ok(json!({"messages":messages(&packet,true)?,"packet":packet}))
}
