//! Model tool calls and evidence admitted into the next model turn.
mod bindings;
use regex::Regex;
use serde_json::{json, Value};
use std::{collections::HashSet, sync::OnceLock};

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn tool_calls(request: &Value) -> Result<Value, &'static str> {
    static FENCE: OnceLock<Regex> = OnceLock::new();
    let allowed = request["allowed"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let allowed: HashSet<_> = allowed.iter().filter_map(Value::as_str).collect();
    let limit = request["max_calls"]
        .as_u64()
        .ok_or("HARNESS_REQUEST_INVALID")?
        .max(1);
    let raw = text(&request["text"]).trim();
    let fence =
        FENCE.get_or_init(|| Regex::new(r"(?is)\x60{3}(?:json)?\s*(\{.*?\})\s*\x60{3}").unwrap());
    let raw = fence
        .captures(raw)
        .and_then(|found| found.get(1))
        .map_or(raw, |found| found.as_str());
    let Some((start, end)) = raw
        .find('{')
        .zip(raw.rfind('}'))
        .filter(|(start, end)| start < end)
    else {
        return Ok(json!({"calls":[]}));
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw[start..=end]) else {
        return Ok(json!({"calls":[]}));
    };
    let calls = &parsed["calls"];
    let items = if calls.is_object() {
        std::slice::from_ref(calls)
    } else {
        calls.as_array().map(Vec::as_slice).unwrap_or_default()
    };
    let mut result = Vec::new();
    for item in items {
        let tool = text(&item["tool"]);
        let tool = if tool.is_empty() {
            text(&item["name"])
        } else {
            tool
        }
        .trim();
        if !allowed.contains(tool) {
            continue;
        }
        let args = item["args"].as_object().cloned().unwrap_or_default();
        let mut call = json!({"tool":tool,"args":args});
        let id = [&item["call_id"], &item["id"]]
            .into_iter()
            .find_map(|value| match value {
                Value::String(id) if !id.is_empty() => Some(id.clone()),
                Value::Number(id) if id.as_f64() != Some(0.0) => Some(id.to_string()),
                _ => None,
            });
        if let Some(id) = id {
            call["call_id"] = json!(id);
        }
        result.push(call);
        if result.len() as u64 >= limit {
            break;
        }
    }
    Ok(json!({"calls":result}))
}

fn evidence(request: &Value) -> Result<Value, &'static str> {
    let existing = request["existing"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let proposed = request["proposed"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let exact = request["exact"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let exact: HashSet<_> = exact
        .iter()
        .map(|key| (text(&key[0]), text(&key[1])))
        .collect();
    let mut known: HashSet<_> = existing.iter().map(text).collect();
    let mut selected = Vec::new();
    for (index, item) in proposed.iter().enumerate() {
        let id = item["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("HARNESS_REQUEST_INVALID")?;
        if !exact.contains(&(text(&item["dataset"]), text(&item["file"]))) && known.insert(id) {
            selected.push(index);
        }
    }
    Ok(json!({"indexes":selected}))
}

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    match request["action"].as_str() {
        Some("parse_tool_calls") => tool_calls(request),
        Some("merge_tool_evidence") => evidence(request),
        Some("bind_pdf_source") => bind_pdf_source(request),
        Some("augment_tool_args") => bindings::augment(request),
        Some("source_read_outcome") => source_read_outcome(request),
        Some("pdf_read_coverage") => pdf_read_coverage(request),
        _ => Err("HARNESS_REQUEST_INVALID"),
    }
}

fn pdf_read_coverage(request: &Value) -> Result<Value, &'static str> {
    let results = request["results"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let mut pages = std::collections::BTreeMap::new();
    let mut identities = std::collections::BTreeMap::new();
    let mut revisions = std::collections::BTreeMap::new();
    for result in results
        .iter()
        .filter(|v| v["tool"] == "read_pdf_source" && v["status"] == "ok")
    {
        let v = &result["result"];
        let id = text(&v["doc_id"]);
        let hash = text(&v["source_sha256"]);
        if revisions
            .insert(id, hash)
            .is_some_and(|previous| previous != hash)
        {
            return Ok(json!({"complete":false,"code":"PDF_REVISION_CHANGED"}));
        }
        if v["selection"] == "literal_match" {
            continue;
        }
        let key = (
            text(&v["doc_id"]),
            text(&v["source_sha256"]),
            v["page"].as_u64().ok_or("HARNESS_REQUEST_INVALID")?,
        );
        identities.insert(key, v);
        let start = v["offset"].as_u64().ok_or("HARNESS_REQUEST_INVALID")?;
        let total = v["total_chars"].as_u64().ok_or("HARNESS_REQUEST_INVALID")?;
        let end = start + text(&v["text"]).chars().count() as u64;
        let (count, ranges) = pages.entry(key).or_insert_with(|| (total, Vec::new()));
        if *count != total || end > total {
            return Err("HARNESS_REQUEST_INVALID");
        }
        ranges.push((start, end));
    }
    let mut continuation = Value::Null;
    let complete = pages.iter_mut().all(|(key,(total, ranges))| {
        ranges.sort_unstable();
        let mut through = 0;
        for &(start, end) in ranges.iter() {
            if start > through {
                break;
            }
            through = through.max(end);
        }
        if through<*total {
            let value=identities[key];
            continuation=json!({"tool":"read_pdf_source","args":{
                "doc_id":value["doc_id"],"dataset_id":value["dataset_id"],"doc_name":value["doc_name"],
                "page":value["page"],"source_sha256":value["source_sha256"],"offset":through,"max_chars":6000},
                "origin":"native_pdf_cursor"});
            continuation["progress_label"]=json!(format!("Дочитываю страницу {}: {} из {} символов",value["page"],through,total));
            false
        } else {true}
    });
    Ok(json!({"complete":complete,"pages":pages.len(),"continuation":continuation}))
}

fn source_read_outcome(request: &Value) -> Result<Value, &'static str> {
    let result = &request["result"];
    let read = matches!(
        text(&result["tool"]),
        "read_source"
            | "read_pdf_source"
            | "read_excel_source"
            | "look_at_pdf_page"
            | "filesystem_read_text"
            | "filesystem_stat"
            | "filesystem_hash"
    );
    let allowed = !read || result["status"] == "ok";
    let code = text(&result["result"]["code"]);
    Ok(
        json!({"allowed":allowed,"code":if code.starts_with("PDF_")||code.starts_with("FILESYSTEM_"){code}else{"SOURCE_READ_FAILED"}}),
    )
}

#[cfg(test)]
mod source_tests {
    use super::*;
    #[test]
    fn continuation_is_the_first_gap_in_the_same_document_and_revision() {
        let part = |start, text| {
            json!({"tool":"read_pdf_source","status":"ok","result":{
            "doc_id":"doc","dataset_id":"dataset","doc_name":"folder/source.pdf","source_sha256":"a".repeat(64),
            "page":5,"offset":start,"total_chars":9,"text":text}})
        };
        let mut request =
            json!({"action":"pdf_read_coverage","results":[part(0,"абв"),part(6,"жзи")]});
        let result = execute(&request).unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(result["continuation"]["args"]["offset"], 3);
        assert_eq!(
            result["continuation"]["args"]["source_sha256"],
            "a".repeat(64)
        );
        assert_eq!(result["continuation"]["args"]["doc_id"], "doc");
        request["results"]
            .as_array_mut()
            .unwrap()
            .push(part(3, "где"));
        let result = execute(&request).unwrap();
        assert_eq!(result["complete"], true);
        assert!(result["continuation"].is_null());
    }
    #[test]
    fn missing_read_cannot_become_successful_terminal_answer() {
        for status in ["missing", "error", "blocked", "partial"] {
            assert_eq!(execute(&json!({"action":"source_read_outcome","result":{"tool":"read_pdf_source","status":status}})).unwrap()["allowed"],false);
        }
        assert_eq!(execute(&json!({"action":"source_read_outcome","result":{"tool":"read_pdf_source","status":"ok"}})).unwrap()["allowed"],true);
    }
    #[test]
    fn bind_only_unique_provenance_and_preserve_requested_revision() {
        let target =
            json!({"match_status":"matched","dataset_id":"uuid","file_name":"folder/Файл.pdf"});
        let mut request = json!({"action":"bind_pdf_source","target":target,"sources":[{"dataset_id":"uuid","doc_name":"folder/Файл.pdf","source_sha256":"a".repeat(64)}],
            "call":{"tool":"read_pdf_source","args":{"doc_id":"Файл.pdf","dataset_id":"folder","page":6}}});
        let call = execute(&request).unwrap();
        assert_eq!(call["args"]["dataset_id"], "uuid");
        assert!(call["args"].get("doc_id").is_none());
        assert_eq!(call["args"]["page"], 6);
        assert_eq!(call["args"]["source_sha256"], "a".repeat(64));
        request["call"]["args"]["doc_id"] = json!("Файл");
        assert_eq!(execute(&request).unwrap()["args"]["dataset_id"], "uuid");
        request["call"]["args"]["doc_name"] = json!("different.pdf");
        assert_eq!(execute(&request).unwrap(), request["call"]);
    }
    #[test]
    fn page_coverage_requires_no_gaps_and_never_mixes_revisions() {
        let part = |offset, text, hash| {
            json!({"tool":"read_pdf_source","status":"ok","result":{
            "doc_id":"doc","source_sha256":hash,"page":2,"offset":offset,"total_chars":6,"text":text}})
        };
        let check = |parts| {
            execute(&json!({"action":"pdf_read_coverage","results":parts})).unwrap()["complete"]
                .clone()
        };
        assert_eq!(
            check(vec![part(0, "абв", "one"), part(3, "где", "one")]),
            true
        );
        assert_eq!(
            check(vec![part(0, "абв", "one"), part(3, "где", "two")]),
            false
        );
        assert_eq!(
            check(vec![part(0, "абв", "one"), part(4, "де", "one")]),
            false
        );
        assert_eq!(
            check(vec![part(0, "абв", "one"), part(0, "абв", "one")]),
            false
        );
    }
}

fn bind_pdf_source(request: &Value) -> Result<Value, &'static str> {
    let mut call = request["call"].clone();
    if call["tool"] != "read_pdf_source" {
        return Ok(call);
    }
    let args = call["args"]
        .as_object_mut()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let name = args
        .get("doc_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .replace('\\', "/");
    let id = args
        .get("doc_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let dataset = args
        .get("dataset_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let target = &request["target"];
    let sources = request["sources"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let mut candidates = std::collections::BTreeMap::new();
    for source in sources.iter().chain(std::iter::once(target)) {
        let file = source["doc_name"]
            .as_str()
            .or_else(|| source["file_name"].as_str())
            .unwrap_or("");
        let ds = text(&source["dataset_id"]);
        if file.is_empty() || ds.is_empty() {
            continue;
        }
        let basename = file.rsplit('/').next().unwrap_or(file);
        let stem = basename.strip_suffix(".pdf").unwrap_or(basename);
        let alias = |v: &str| {
            v == file
                || v == basename
                || v == stem
                || (!text(&source["doc_id"]).is_empty() && v == text(&source["doc_id"]))
        };
        let selected = target["match_status"] == "matched"
            && text(&target["dataset_id"]) == ds
            && text(&target["file_name"]) == file;
        // Repair only exact known aliases. Conflicting or unknown files are never substituted.
        if (!name.is_empty() && !alias(&name)) || (!id.is_empty() && !alias(&id)) {
            continue;
        }
        if name.is_empty() && id.is_empty() && !selected {
            continue;
        }
        if !selected && !dataset.is_empty() && dataset != ds {
            continue;
        }
        let key = (ds.to_owned(), file.to_owned());
        let hash = source["source_sha256"]
            .as_str()
            .or_else(|| source["locator"]["source_sha256"].as_str())
            .unwrap_or("");
        let hashes = candidates
            .entry(key)
            .or_insert_with(std::collections::BTreeSet::new);
        if !hash.is_empty() {
            hashes.insert(hash.to_owned());
        }
    }
    if candidates.len() == 1 {
        let ((dataset, file), hashes) = candidates.into_iter().next().unwrap();
        if hashes.len() > 1 && !args.contains_key("source_sha256") {
            return Err("HARNESS_SOURCE_REVISION_AMBIGUOUS");
        }
        args.remove("doc_id");
        args.insert("dataset_id".into(), json!(dataset));
        args.insert("doc_name".into(), json!(file));
        if !args.contains_key("source_sha256") && hashes.len() == 1 {
            args.insert(
                "source_sha256".into(),
                json!(hashes.into_iter().next().unwrap()),
            );
        }
    }
    Ok(call)
}
