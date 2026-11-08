//! Model-neutral harness budget/ordering. No inference or intent heuristics.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
pub(crate) const ORDER: &[&str] = &[
    "profile_prefix",
    "tool_shortlist",
    "request",
    "evidence",
    "source_map",
    "tool_exchange",
    "checkpoint",
    "working_memory",
    "dialogue",
    "native_tool_exchange",
];
type Result<T> = std::result::Result<T, &'static str>;
fn array<'a>(r: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    r[key].as_array().ok_or("CONTEXT_REQUEST_INVALID")
}
fn text<'a>(r: &'a Value, key: &str) -> Result<&'a str> {
    r[key].as_str().ok_or("CONTEXT_REQUEST_INVALID")
}
fn number(r: &Value, key: &str) -> Result<i64> {
    r[key].as_i64().ok_or("CONTEXT_REQUEST_INVALID")
}
fn pack(r: &Value) -> Result<Value> {
    let budget = number(r, "input_limit")?
        .saturating_sub(number(r, "generation_reserve")?)
        .saturating_sub(number(r, "safety_reserve")?)
        .max(0);
    let mut ordered = Vec::new();
    for c in array(r, "candidates")? {
        let kind = text(c, "kind")?;
        let rank = ORDER
            .iter()
            .position(|v| *v == kind)
            .ok_or("CONTEXT_REQUEST_INVALID")?;
        for item in array(c, "objects")? {
            text(item, "id")?;
            text(item, "render")?;
            if number(item, "tokens")? < 0 {
                return Err("CONTEXT_REQUEST_INVALID");
            }
        }
        ordered.push((rank, c));
    }
    ordered.sort_by_key(|(rank, _)| *rank);
    let mut selected: Vec<Vec<Value>> = vec![vec![]; ORDER.len()];
    let mut section_tokens = vec![0i64; ORDER.len()];
    let mut used = 0i64;
    let mut count = 0usize;
    let mut required_ids = Vec::new();
    for (rank, c) in &ordered {
        if c["required"] != true {
            continue;
        }
        for item in array(c, "objects")? {
            required_ids.push(item["id"].clone());
            let tokens = number(item, "tokens")?;
            used = used
                .checked_add(i64::from(count > 0))
                .and_then(|v| v.checked_add(tokens))
                .ok_or("CONTEXT_REQUEST_INVALID")?;
            section_tokens[*rank] = section_tokens[*rank]
                .checked_add(i64::from(!selected[*rank].is_empty()))
                .and_then(|v| v.checked_add(tokens))
                .ok_or("CONTEXT_REQUEST_INVALID")?;
            selected[*rank].push(item.clone());
            count += 1;
        }
    }
    if used > budget {
        return Ok(
            json!({"overflow":true,"object_ids":required_ids,"budget":budget,"required_tokens":used}),
        );
    }
    let mut omissions = Vec::new();
    for (rank, c) in &ordered {
        if c["required"] == true {
            continue;
        }
        let mut omitted = Vec::new();
        let objects = array(c, "objects")?;
        for item in objects {
            let tokens = number(item, "tokens")?;
            let cost = tokens
                .checked_add(i64::from(used > 0))
                .ok_or("CONTEXT_REQUEST_INVALID")?;
            if cost <= budget - used {
                section_tokens[*rank] += i64::from(!selected[*rank].is_empty()) + tokens;
                selected[*rank].push(item.clone());
                used += cost;
            } else {
                omitted.push(item);
            }
        }
        if !omitted.is_empty() {
            let mut digest = Sha256::new();
            let mut ids = Vec::new();
            for item in &omitted {
                let id = text(item, "id")?;
                ids.push(id);
                digest.update(id.as_bytes());
                digest.update([0]);
                digest.update(text(item, "render")?.as_bytes());
                digest.update([0]);
            }
            let hash = format!("{:x}", digest.finalize());
            omissions.push(json!({"kind":ORDER[*rank],"total":objects.len(),"omitted":omitted.len(),"object_ids":ids,"cursor":format!("ctx:{}:{}",ORDER[*rank],&hash[..20]),"reason":"budget_exhausted"}));
        }
    }
    let sections: Vec<_> = ORDER.iter().enumerate().filter(|(i,_)| !selected[*i].is_empty()).map(|(i,kind)| json!({"kind":kind,"indexes":selected[i].iter().map(|v|v["index"].clone()).collect::<Vec<_>>(),"tokens":section_tokens[i]})).collect();
    Ok(json!({"budget":budget,"used":used,"sections":sections,"omissions":omissions}))
}
fn messages(r: &Value) -> Result<Value> {
    let sections = array(r, "sections")?;
    let system: Vec<_> = sections
        .iter()
        .filter(|s| s["kind"] == "profile_prefix")
        .map(|s| text(s, "render"))
        .collect::<Result<_>>()?;
    let system = system.join("\n\n");
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(json!({"role":"system","content":system}));
    }
    if r["request_last"] == true {
        for section in sections.iter().filter(|s| s["kind"] == "dialogue") {
            messages.extend(array(section, "dialogue")?.iter().cloned());
        }
        let context: Vec<_> = sections
            .iter()
            .filter(|s| {
                ![
                    "profile_prefix",
                    "dialogue",
                    "request",
                    "native_tool_exchange",
                ]
                .contains(&s["kind"].as_str().unwrap_or(""))
            })
            .map(|s| text(s, "render"))
            .collect::<Result<_>>()?;
        if !context.is_empty() {
            messages.push(json!({"role":"user","content":context.join("\n")}));
        }
        for section in sections.iter().filter(|s| s["kind"] == "request") {
            messages.push(json!({"role":"user","content":text(section,"render")?}));
        }
    } else {
        let context: Vec<_> = sections
            .iter()
            .filter(|s| {
                !["profile_prefix", "native_tool_exchange"]
                    .contains(&s["kind"].as_str().unwrap_or(""))
            })
            .map(|s| text(s, "render"))
            .collect::<Result<_>>()?;
        if !context.is_empty() {
            messages.push(json!({"role":"user","content":context.join("\n")}));
        }
    }
    for section in sections
        .iter()
        .filter(|s| s["kind"] == "native_tool_exchange")
    {
        messages.extend(array(section, "native")?.iter().cloned());
    }
    Ok(json!({"messages":messages}))
}
pub fn execute(r: &Value) -> Result<Value> {
    match text(r, "action")? {
        "pack" => pack(r),
        "messages" => messages(r),
        _ => Err("CONTEXT_REQUEST_INVALID"),
    }
}
