use les_core::index_publication::Writer;
use les_vector::{Shard, migration};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn setup(legacy: bool) -> (Fixture, PathBuf, Vec<Value>) {
    let root = std::env::temp_dir().join(format!(
        "les-edge-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let mut rows = (0..4).map(|i| json!({"id":i,"vector":{"dense":[0.6,0.8,0.]},
        "payload":{"dataset_id":"project","file_name":if i<2{"first.pdf"}else{"second.pdf"},
        "node_role":"evidence","text":if i<2{"Прежняя редакция кабелепровода"}else{"Независимый документ вентиляции"},"page":1.25}})).collect::<Vec<_>>();
    if legacy {
        for row in &mut rows {
            row["payload"].as_object_mut().unwrap().remove("node_role");
        }
    }
    let source = root.join("source.jsonl");
    fs::write(
        &source,
        rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
    let manifest = root.join("manifest.json");
    fs::write(
        &manifest,
        json!({"schema":"les.edge-import.v1","collection":"docs","source":source,
        "source_sha256":les_vector::digest(&source).unwrap(),"points":rows.len(),
        "vectors":{"dense":{"size":3,"distance":"Cosine"}},"sparse_vectors":{}})
        .to_string(),
    )
    .unwrap();
    let target = root.join("edge");
    migration::import(&manifest, &target, |_| {}).unwrap();
    (Fixture(root), target, rows)
}
fn search(shard: &Shard, text: &str) -> Value {
    shard
        .call(
            "hybrid_search",
            &json!({"text":text,"vector":[0.6,0.8,0.],"dense_name":"dense",
        "limit":4,"filters":{"dataset_ids":["project"]}}),
        )
        .unwrap()
}
#[test]
fn file_publication_rebuilds_only_changed_scope_and_rejects_inflight_generation() {
    let (_fixture, target, mut rows) = setup(false);
    let shard = Shard::open(&target).unwrap();
    let first = search(&shard, "кабелепровод");
    let stamp = first["revision"].clone();
    let writer = Writer::acquire(&shard.content_root(), "docs").unwrap();
    writer.journal("begin",json!({"dataset":"project","file_name":"first.pdf","db_file_name":"first.pdf","ids":["replacement"]})).unwrap();
    rows[0]["payload"]["text"] = json!("Новая редакция: сверхпроводимость");
    shard
        .call("upsert", &json!({"points":[rows[0].clone()]}))
        .unwrap();
    drop(writer); // Simulate a stopped publisher. Readers must not accept half a revision.
    assert_eq!(
        shard
            .call(
                "hybrid_search",
                &json!({"text":"кабель","vector":[0.6,0.8,0.],"dense_name":"dense","limit":4})
            )
            .unwrap_err(),
        "INDEX_RECOVERY_REQUIRED"
    );
    let writer = Writer::acquire(&shard.content_root(), "docs").unwrap();
    writer.journal("commit", json!({})).unwrap();
    writer.journal("finish", json!({})).unwrap();
    drop(writer);
    let mut processed = 0;
    shard
        .refresh_postings(false, |v| processed = v["completed"].as_u64().unwrap())
        .unwrap();
    assert_eq!(processed, 2); // The second file was not tokenized or rewritten.
    assert_ne!(search(&shard, "сверхпроводимость")["revision"], stamp);
    let lexical = shard
        .lexical_search("сверхпроводимость", &json!({}), 4)
        .unwrap();
    assert_eq!(lexical["hits"][0][0], "0");
    assert_eq!(
        shard.lexical_search("вентиляции", &json!({}), 4).unwrap()["hits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(shard.call("hybrid_search",&json!({"text":"кабель","vector":[0.6,0.8,0.],"dense_name":"dense","limit":4,"revision":stamp})).unwrap_err(),"INDEX_CHANGED_DURING_SEARCH");
    // Float range filters must survive the arbitrary-precision JSON boundary.
    assert_eq!(
        shard
            .call(
                "count",
                &json!({"filter":{"must":[{"key":"page","range":{"gte":1.2,"lt":1.3}}]}})
            )
            .unwrap()["count"],
        4
    );
}

#[test]
fn hierarchy_keeps_legacy_evidence_without_navigation_metadata_globally_searchable() {
    let (_fixture, target, rows) = setup(true);
    let shard = Shard::open(&target).unwrap();
    let result = shard
        .hybrid_search(
            &json!({"text":"вентиляции","vector":[0.6,0.8,0.],"dense_name":"dense",
        "limit":4,"hierarchy":true,"filters":{"dataset_ids":["project"]}}),
        )
        .unwrap();
    assert_eq!(result["points"].as_array().unwrap().len(), rows.len());
    let lexical = shard
        .lexical_search("вентиляции", &json!({"node_roles":["evidence",""]}), 4)
        .unwrap();
    assert_eq!(lexical["hits"].as_array().unwrap().len(), 2);
}
