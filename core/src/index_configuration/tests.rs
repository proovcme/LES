use super::*;
use std::path::PathBuf;
fn tokenizer() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "les-tokenizer-policy-{}-{}.json",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    std::fs::write(&path,br#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#).unwrap();
    path
}
fn sign(value: &mut Value) {
    value.as_object_mut().unwrap().remove("fingerprint");
    value.sort_all_objects();
    value["fingerprint"] = json!(crate::sha256_hex(value.to_string().as_bytes()));
}
#[test]
fn metadata_contract_is_explicit_and_ollama_does_not_inherit_coreml_limit() {
    let path = tokenizer();
    let tokenizer = json!({"path":path});
    let settings = json!({"LES_EMBED_PROFILE":"quality","EMBED_BACKEND":"ollama","COREML_EMBED_SEQ_LEN":"512"});
    let mut contract = crate::index_bootstrap::contract(
        &settings,
        "bge-m3:latest",
        "native-revision",
        1024,
        "ollama",
        tokenizer.clone(),
    )
    .unwrap();
    assert_eq!(contract["document_embedding_mode"], "metadata-v1");
    assert_eq!(
        contract["chunker_revision"],
        "canonical-source-final-budget-v4"
    );
    assert_eq!(contract["chunk_size"], 512);
    assert_eq!(contract["chunk_overlap"], 64);
    let prepared =
        execute(&json!({"settings":settings,"contract":contract,"tokenizer":tokenizer})).unwrap();
    assert_eq!(prepared["node_preparation"]["structural"], true);
    assert_eq!(prepared["node_preparation"]["size"], 512);
    contract["chunker_revision"] = json!("structure-aware-final-budget-v2");
    sign(&mut contract);
    assert!(
        execute(&json!({"settings":settings,"contract":contract,"tokenizer":tokenizer})).is_err()
    );
    contract["document_embedding_mode"] = json!("raw-v1");
    contract["chunk_size"] = json!(430);
    contract["chunk_overlap"] = json!(50);
    sign(&mut contract);
    let legacy =
        execute(&json!({"settings":settings,"contract":contract,"tokenizer":tokenizer})).unwrap();
    assert_eq!(legacy["node_preparation"]["structural"], false);
    std::fs::remove_file(path).unwrap();
}
