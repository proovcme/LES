#[test]
fn rejected_draft_keeps_tools_available_without_replenishing_call_budget() {
    let mut agent = super::Coordinator::new(3, 30., ["read_source".into()]).unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        super::Step::Model {
            final_answer: false
        }
    ));
    let remaining = agent.remaining();
    agent.retry_model("read_required").unwrap();
    assert_eq!(agent.remaining(), remaining);
    assert!(matches!(
        agent.next_blocking().unwrap(),
        super::Step::Model {
            final_answer: false
        }
    ));
    agent.model_result(serde_json::json!({"tool_calls":[{"function":{"name":"read_source","arguments":{"doc_id":"doc"}}}]})).unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        super::Step::Tool(_)
    ));
    assert!(agent.retry_model("wrong_phase").is_err());
    agent.tool_result(serde_json::json!({"tool":"read_source","status":"ok","result":{"text":"verified receipt"}})).unwrap();
    assert_eq!(agent.remaining(), remaining - 1);
}
#[test]
fn an_unsent_source_step_can_close_once_without_replenishing_actions() {
    let mut agent = Coordinator::new(3, 60., ["read_source".into()]).unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model {
            final_answer: false
        }
    ));
    let remaining = agent.remaining();
    agent.prepare_answer_window().unwrap();
    assert_eq!(agent.remaining(), remaining);
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model { final_answer: true }
    ));
    assert!(agent.prepare_answer_window().is_err());
    agent
        .model_result(json!({"text":"Confirmed sources"}))
        .unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Complete(_)));
    agent.revise_completion().unwrap();
    assert_eq!(agent.remaining(), remaining);
}
#[test]
fn preparing_the_answer_window_keeps_one_bounded_format_repair() {
    let mut agent = Coordinator::new(3, 60., ["read_source".into()]).unwrap();
    // An unsent model step is allowed; an idle loop without a step is not.
    assert!(agent.prepare_answer_window().is_err());
    agent.next_blocking().unwrap();
    agent
        .model_result(json!({"text":"Partial window"}))
        .unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Complete(_)));
    let remaining = agent.remaining();
    agent.prepare_answer_window().unwrap();
    assert_eq!(agent.remaining(), remaining);
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model { final_answer: true }
    ));
    agent
        .model_result(json!({"text":"Invalid selection in the full window"}))
        .unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Complete(_)));
    assert!(agent.prepare_answer_window().is_err());
    agent.revise_completion().unwrap();
    assert_eq!(agent.remaining(), remaining);
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model { final_answer: true }
    ));
    agent
        .model_result(json!({"text":"Corrected selection"}))
        .unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Complete(_)));
    assert!(agent.revise_completion().is_err());
    assert!(agent.prepare_answer_window().is_err());
    assert_eq!(agent.remaining(), remaining);
}
#[test]
fn completion_format_revision_is_once_and_keeps_the_tool_budget_closed() {
    let mut agent = super::Coordinator::new(1, 30., ["search_sources".into()]).unwrap();
    agent.next_blocking().unwrap();
    agent
        .model_result(serde_json::json!({"text":"Draft"}))
        .unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        super::Step::Complete(_)
    ));
    let remaining = agent.remaining();
    agent.revise_completion().unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        super::Step::Model { final_answer: true }
    ));
    assert_eq!(agent.remaining(), remaining);
    agent
        .model_result(serde_json::json!({"text":"Cited revision [Источник 1]"}))
        .unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        super::Step::Complete(_)
    ));
    assert_eq!(
        agent.revise_completion(),
        Err("AGENT_COMPLETION_REVISION_INVALID")
    );
    agent.fail("TOOL_FAILED");
    assert!(agent.revise_completion().is_err());
}
use super::*;
fn propose(agent: &mut Coordinator, args: Value) {
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model {
            final_answer: false
        }
    ));
    agent.model_result(json!({"tool_calls":[{"id":"call","function":{"name":"use_extensions_fixture","arguments":args}}]})).unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Tool(_)));
}
fn extension(inner: Value) -> Value {
    json!({"tool":"use_extensions_fixture","operation":"call","status":"ok","result":inner})
}
#[test]
fn delivered_review_span_allows_retrying_an_earlier_rejected_record_through_extensions() {
    let mut agent = Coordinator::new(
        8,
        30.,
        ["use_extensions_fixture".into(), "document_review".into()],
    )
    .unwrap();
    agent
        .capability_revision("document_review", json!(["task", 0, 0]))
        .unwrap();
    let save =
        json!({"operation":"call","name":"document_review","arguments":{"operation":"record"}});
    propose(&mut agent, save.clone());
    agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"document_review","operation":"record","status":"error"}))).unwrap();
    propose(
        &mut agent,
        json!({"operation":"call","name":"document_review","arguments":{"operation":"read"}}),
    );
    agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"document_review","operation":"read","status":"ok"}))).unwrap();
    // The runtime acknowledges the complete context only after successful model IO.
    agent
        .capability_revision("document_review", json!(["task", 0, 1]))
        .unwrap();
    propose(&mut agent, save);
    assert!(agent.capability_revision("not-granted", json!(1)).is_err());
}
#[test]
fn wrapped_workspace_write_invalidates_prior_read_identity_and_survives_table_epoch_update() {
    let mut agent = Coordinator::new(
        8,
        30.,
        [
            "use_extensions_fixture".into(),
            "agent_workspace".into(),
            "table_document".into(),
        ],
    )
    .unwrap();
    let read =
        json!({"operation":"call","name":"agent_workspace","arguments":{"operation":"read"}});
    propose(&mut agent, read.clone());
    agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"agent_workspace","status":"ok","result":{"changed":false}}))).unwrap();
    propose(
        &mut agent,
        json!({"operation":"call","name":"agent_workspace","arguments":{"operation":"save"}}),
    );
    agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"agent_workspace","status":"ok","result":{"changed":true}}))).unwrap();
    agent.update_epochs(json!({"table_document":7})).unwrap();
    propose(&mut agent, read);
}
#[test]
fn wrapped_table_commit_allows_reading_the_next_packet() {
    let mut agent = Coordinator::new(
        8,
        30.,
        ["use_extensions_fixture".into(), "table_document".into()],
    )
    .unwrap();
    agent.update_epochs(json!({"table_document":0})).unwrap();
    let next = json!({"operation":"call","name":"table_document","arguments":{"operation":"next"}});
    propose(&mut agent, next.clone());
    agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"table_document","status":"ok","result":{"revision":0}}))).unwrap();
    propose(
        &mut agent,
        json!({"operation":"call","name":"table_document","arguments":{"operation":"record"}}),
    );
    agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"table_document","status":"ok","result":{"revision":1}}))).unwrap();
    agent.update_epochs(json!({"table_document":1})).unwrap();
    propose(&mut agent, next);
}
#[test]
fn failed_file_read_never_requests_a_confident_followup_model_answer() {
    let mut agent = Coordinator::new(4, 30., ["filesystem_read_text".into()]).unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Model { .. }));
    agent.model_result(json!({"text":"","tool_calls":[{"id":"file-1","function":{"name":"filesystem_read_text","arguments":{"path":"../outside.txt"}}}]})).unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Tool(_)));
    agent.tool_result(json!({"tool":"filesystem_read_text","status":"error","result":{"code":"FILESYSTEM_SCOPE_DENIED"}})).unwrap();
    assert!(
        matches!(agent.next_blocking().unwrap(),Step::Failed(code) if code=="FILESYSTEM_SCOPE_DENIED")
    );
}
#[test]
fn a_read_failure_never_requests_a_successful_final_answer() {
    let mut agent = Coordinator::new(4, 30., ["read_pdf_source".into()]).unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model {
            final_answer: false
        }
    ));
    agent.model_result(json!({"text":"","tool_calls":[{"function":{"name":"read_pdf_source","arguments":"{\"page\":3}"}}]})).unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Tool(_)));
    agent.tool_result(json!({"tool":"read_pdf_source","status":"error","result":{"code":"PDF_REVISION_CHANGED"}})).unwrap();
    assert!(
        matches!(agent.next_blocking().unwrap(),Step::Failed(code) if code=="PDF_REVISION_CHANGED")
    );
    assert_eq!(agent.results().len(), 1);
}
#[test]
fn duplicate_calls_are_not_executed_twice_and_results_survive_budget_end() {
    let mut agent = Coordinator::new(1, 30., ["search_sources".into()]).unwrap();
    agent.next_blocking().unwrap();
    agent.model_result(json!({"tool_calls":[{"function":{"name":"search_sources","arguments":"{\"q\":\"test\"}"}},{"function":{"name":"search_sources","arguments":"{\"q\":\"test\"}"}}]})).unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Tool(_)));
    agent
        .tool_result(json!({"tool":"search_sources","status":"ok","result":{}}))
        .unwrap();
    assert!(matches!(
        agent.next_blocking().unwrap(),
        Step::Model { final_answer: true }
    ));
    agent
        .model_result(json!({"text":"answer","tool_calls":[]}))
        .unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Complete(_)));
    assert_eq!(agent.remaining(), 0);
}
#[test]
fn incomplete_pages_are_read_by_exact_cursor_before_calling_the_model_again() {
    let mut agent = Coordinator::new(3, 30., ["read_pdf_source".into()]).unwrap();
    agent.next_blocking().unwrap();
    agent
        .model_result(
            json!({"tool_calls":[{"function":{"name":"read_pdf_source","arguments":{"page":1}}}]}),
        )
        .unwrap();
    agent.next_blocking().unwrap();
    agent.tool_result(json!({"tool":"read_pdf_source","status":"ok","result":{"doc_id":"doc","dataset_id":"ds","doc_name":"test.pdf","source_sha256":"hash","page":1,"offset":0,"total_chars":4,"text":"ab","complete":false}})).unwrap();
    let Step::Tool(call) = agent.next_blocking().unwrap() else {
        panic!("Model was called before completing the page")
    };
    assert_eq!(call["args"]["offset"], 2);
    assert_eq!(call["args"]["source_sha256"], "hash");
}
#[test]
fn a_provider_id_prefix_cannot_hide_a_real_tool_result() {
    let mut agent = Coordinator::new(3, 30., ["read_source".into()]).unwrap();
    agent.next_blocking().unwrap();
    agent.model_result(json!({"tool_calls":[{"id":"les-cursor-provider","function":{"name":"read_source","arguments":{"doc_id":"d"}}}]})).unwrap();
    assert!(matches!(agent.next_blocking().unwrap(), Step::Tool(_)));
    agent
        .tool_result(
            json!({"tool":"read_source","status":"ok","result":{"text":"Visible evidence"}}),
        )
        .unwrap();
    agent.next_blocking().unwrap();
    let messages = agent.provider_messages(true);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "les-cursor-provider");
}

#[test]
fn legacy_selection_results_do_not_fabricate_provider_tool_calls() {
    let mut agent = Coordinator::new(3, 30., ["read_source".into()]).unwrap();
    agent.next_blocking().unwrap();
    agent.model_result(json!({"selection_text":"{\"calls\":[{\"tool\":\"read_source\",\"args\":{\"doc_id\":\"d\"}}]}"})).unwrap();
    let Step::Tool(call) = agent.next_blocking().unwrap() else {
        panic!("Missing tool")
    };
    assert!(call["call_id"].is_null());
    agent
        .tool_result(json!({"tool":"read_source","status":"ok","result":{"text":"Read data"}}))
        .unwrap();
    agent.next_blocking().unwrap();
    let messages = agent.provider_messages(true);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
}
