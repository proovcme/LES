//! Stable synchronous ABI port into the same Rho runtime used by the public API.
use super::{Coordinator, Step};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
pub fn run_research(
    request: &Value,
    bridge: impl Fn(u8, &Value) -> Result<Value, &'static str>,
) -> Result<Value, &'static str> {
    let answer = request["schema"] == "les.chat-answer.v1";
    if !answer && request["schema"] != "les.chat-research.v1" {
        return Err("AGENT_REQUEST_INVALID");
    }
    let allowed = if answer {
        Vec::new()
    } else {
        request["allowed_tools"]
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or("AGENT_REQUEST_INVALID")?
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 120)
                    .map(str::to_owned)
                    .ok_or("AGENT_REQUEST_INVALID")
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let maximum = if answer {
        0
    } else {
        request["max_calls"]
            .as_u64()
            .ok_or("AGENT_REQUEST_INVALID")?
    };
    let seconds = request["deadline_seconds"]
        .as_f64()
        .ok_or("AGENT_REQUEST_INVALID")?;
    let mut agent = Coordinator::new(maximum, seconds, allowed)?;
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut binding = request["binding"].clone();
    let mut attempts = 0;
    if !answer && !binding["dataset_ids"].is_array() {
        return Err("AGENT_CONTEXT_INVALID");
    }
    let max_attempts = if answer {
        request["max_attempts"]
            .as_u64()
            .filter(|v| (1..=2).contains(v))
            .ok_or("AGENT_REQUEST_INVALID")?
    } else {
        1
    };
    loop {
        let step = agent.next_blocking()?;
        let (operation, mut input) = match step {
            Step::Model { final_answer: true } if !answer => {
                return Ok(
                    json!({"status":"needs_final","rounds":agent.rounds,"remaining":agent.remaining(),"stop_reason":agent.stop_reason(),"results":agent.results()}),
                )
            }
            Step::Model { .. } => {
                attempts += 1;
                (
                    0,
                    if answer {
                        json!({"attempt":attempts,"strict":attempts>1})
                    } else {
                        json!({"round":agent.rounds.len()+1,"remaining":agent.remaining()})
                    },
                )
            }
            Step::Tool(call) => (1, json!({"call":call})),
            Step::Complete(terminal) => {
                agent
                    .accept_completed_blocking(terminal.clone())
                    .map_err(|_| "RHO_COMPLETION_NOT_FINAL")?;
                return Ok(if answer {
                    json!({"status":"completed","attempts":attempts,"terminal":terminal})
                } else {
                    json!({"status":"completed","terminal":terminal,"rounds":agent.rounds,"remaining":agent.remaining(),"stop_reason":agent.stop_reason(),"results":agent.results()})
                });
            }
            Step::Failed(code) => {
                return Ok(
                    json!({"status":"failed","code":if answer&&code=="RESEARCH_DEADLINE_EXCEEDED"{"ANSWER_DEADLINE_EXCEEDED"}else{&code},"rounds":agent.rounds,"remaining":agent.remaining(),"stop_reason":agent.stop_reason(),"results":agent.results()}),
                )
            }
            Step::Finished => return Err("RHO_COMPLETION_NOT_FINAL"),
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        input["timeout_ms"] = json!(remaining.as_millis().min(u64::MAX as u128) as u64);
        if remaining.is_zero() {
            agent.fail(if answer {
                "ANSWER_DEADLINE_EXCEEDED"
            } else {
                "RESEARCH_DEADLINE_EXCEEDED"
            });
            continue;
        }
        let value = bridge(operation, &input)?;
        if let Some(e) = value.get("error") {
            agent.fail(e["code"].as_str().unwrap_or("CHAT_IO_FAILED"));
            continue;
        }
        if let Some(epochs) = value.get("epochs") {
            agent.set_epochs(epochs.clone())?;
        }
        if operation == 0 {
            if answer && (!value["response"].is_object() || !value["response"]["text"].is_string())
            {
                return Err("UPSTREAM_RESPONSE_INVALID");
            }
            if answer
                && value["response"]
                    .get("tool_calls")
                    .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|v| !v.is_empty()))
            {
                agent.fail("TOOL_BUDGET_EXHAUSTED");
                continue;
            }
            if answer
                && value["response"]["text"]
                    .as_str()
                    .is_some_and(|s| s.trim().is_empty())
                && attempts < max_attempts
            {
                // A bounded answer retry is a Rho control capability, not a second loop.
                agent.reply(super::Decision::Retry)?;
                continue;
            }
            binding["sources"] = value["sources"].clone();
            agent.model_result_bound(
                value["response"].clone(),
                if answer { None } else { Some(&binding) },
            )?;
        } else {
            agent.tool_result(value["payload"].clone())?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    fn request() -> Value {
        json!({"schema":"les.chat-research.v1","allowed_tools":["read_pdf_source","agent_workspace"],
        "max_calls":4,"deadline_seconds":10.,"binding":{"question":"Read the page","dataset_ids":["ds"],"target":{},"sources":[]}})
    }
    fn page(start: u64, end: u64) -> Value {
        json!({"tool":"read_pdf_source","status":"ok","result":{"doc_id":"doc","dataset_id":"ds","doc_name":"Doc.pdf",
        "source_sha256":"a".repeat(64),"page":1,"offset":start,"text":"x".repeat((end-start) as usize),"total_chars":12,
        "next_offset":if end<12{json!(end)}else{Value::Null},"complete":end==12}})
    }
    #[test]
    fn legacy_selection_and_native_pdf_cursor_are_driven_without_an_extra_model_turn() {
        let model_calls = RefCell::new(0);
        let offsets = RefCell::new(Vec::new());
        let result=run_research(&request(),|operation,value|{
            if operation==0 {
                *model_calls.borrow_mut()+=1;
                let selection=if *model_calls.borrow()==1 {r#"{"calls":[{"tool":"read_pdf_source","args":{"doc_id":"doc","page":1,"offset":0}}]}"#}else{"Final evidence answer"};
                Ok(json!({"response":{"text":selection,"tool_calls":[],"selection_text":selection},"epochs":{},"sources":[]}))
            }else{
                let start=value["call"]["args"]["offset"].as_u64().unwrap_or(0);
                offsets.borrow_mut().push(start);
                Ok(json!({"payload":page(start,if start==0{6}else{12}),"epochs":{}}))
            }
        }).unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(*model_calls.borrow(), 2);
        assert_eq!(*offsets.borrow(), vec![0, 6]);
        assert_eq!(result["results"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn source_failure_prevents_a_final_model_answer() {
        let model_calls = RefCell::new(0);
        let result=run_research(&request(),|operation,_|{
            if operation==0 {
                *model_calls.borrow_mut()+=1;
                Ok(json!({"response":{"text":"","selection_text":r#"{"calls":[{"tool":"read_pdf_source","args":{"doc_id":"doc","page":1}}]}"#,
                    "tool_calls":[]},"sources":[],"epochs":{}}))
            }else{Ok(json!({"payload":{"tool":"read_pdf_source","status":"error","result":{"code":"PDF_REVISION_CHANGED"}},"epochs":{}}))}
        }).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(result["code"], "PDF_REVISION_CHANGED");
        assert_eq!(*model_calls.borrow(), 1);
    }
    #[test]
    fn empty_or_replayed_calls_defer_final_generation_without_repeating_effects() {
        let model_calls = RefCell::new(0);
        let tool_calls = RefCell::new(0);
        let result=run_research(&request(),|operation,_|{
            if operation==0 {
                *model_calls.borrow_mut()+=1;
                Ok(json!({"response":{"text":"","selection_text":r#"{"calls":[{"tool":"agent_workspace","args":{"action":"read","path":"note.md"}}]}"#,
                    "tool_calls":[]},"sources":[],"epochs":{}}))
            }else{*tool_calls.borrow_mut()+=1;Ok(json!({"payload":{"tool":"agent_workspace","status":"ok","result":{}},"epochs":{}}))}
        }).unwrap();
        assert_eq!(result["status"], "needs_final");
        assert_eq!(*tool_calls.borrow(), 1);
        assert_eq!(*model_calls.borrow(), 2);
    }
}
