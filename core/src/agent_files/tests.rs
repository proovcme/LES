use super::*;
fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("les-agent-test-{}", identifier().unwrap()));
    fs::create_dir_all(&path).unwrap();
    path
}
fn call(root: &Path, action: &str, args: Value) -> Value {
    execute(
        &json!({"schema":"les.agent-files.v1","state_root":root,"database":root.join("data/meta.db"),"action":action,"arguments":args}),
    )
}
fn args(text: &str, revision: &str) -> Value {
    json!({"area":"memory","project_id":0,"text":text,"expected_revision":revision,"actor":"agent","reason":"test"})
}

#[test]
fn clean_read_never_creates_files_and_unicode_roundtrip_undo() {
    let root = root();
    let empty = call(&root, "bundle", json!({"project_id":0}));
    assert_eq!(empty["value"]["documents"].as_array().unwrap().len(), 2);
    assert!(!root.join("agent").exists());
    let saved = call(&root, "save", args("  日本語 🌲\r\nТекст  ", "missing"));
    assert_eq!(saved["status"], "completed");
    let read = call(&root, "read", json!({"project_id":0,"area":"memory"}));
    assert_eq!(read["value"]["text"], "  日本語 🌲\r\nТекст  ");
    assert_eq!(
        call(&root, "save", args("stale", "missing"))["code"],
        "AGENT_CONFLICT"
    );
    let restored = call(
        &root,
        "restore",
        json!({"area":"memory","project_id":0,"history_id":saved["value"]["history_id"],"side":"before","expected_revision":saved["value"]["revision"]}),
    );
    assert_eq!(restored["status"], "completed");
    assert!(!root.join("agent/MEMORY.md").exists());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn paths_and_policy_cannot_escape_or_be_changed_by_an_agent() {
    let root = root();
    for path in [
        "../outside.md",
        "/outside.md",
        "C:/outside.md",
        "a\\b",
        "a/../x",
        "NUL.txt",
        "a.",
        "a:secret",
        ".env",
        "a//b",
    ] {
        let result = call(
            &root,
            "save",
            json!({"project_id":0,"area":"files","path":path,"text":"x","expected_revision":"missing","actor":"agent"}),
        );
        assert_eq!(result["code"], "AGENT_PATH_UNSAFE", "{path}");
    }
    let policy = call(
        &root,
        "policy_save",
        json!({"project_id":0,"area":"policy","expected_revision":"missing","value":{"auto_memory":false,"workspace_write":false},"actor":"user"}),
    );
    assert_eq!(policy["status"], "completed");
    assert_eq!(
        call(&root, "save", args("forbidden", "missing"))["code"],
        "AGENT_DISABLED"
    );
    assert_eq!(
        call(
            &root,
            "policy_save",
            json!({"project_id":0,"area":"policy","expected_revision":policy["value"]["revision"],"value":{"auto_memory":true,"workspace_write":true},"actor":"agent"})
        )["code"],
        "AGENT_DISABLED"
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn invalid_encoding_is_preserved() {
    let root = root();
    fs::create_dir(root.join("agent")).unwrap();
    fs::write(root.join("agent/MEMORY.md"), [0xff, 0xfe, 0x80]).unwrap();
    assert_eq!(
        call(&root, "read", json!({"area":"memory","project_id":0}))["code"],
        "AGENT_ENCODING"
    );
    assert_eq!(
        fs::read(root.join("agent/MEMORY.md")).unwrap(),
        [0xff, 0xfe, 0x80]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bounded_edit_changes_only_one_exact_fragment() {
    let root = root();
    let save = call(
        &root,
        "save",
        args("Начало\n\nСтарый факт 日本語\n\nКонец", "missing"),
    );
    let edited = call(
        &root,
        "edit",
        json!({"project_id":0,"area":"memory","old_text":"Старый факт 日本語","new_text":"Новый факт 🌲","expected_revision":save["value"]["revision"],"actor":"agent"}),
    );
    assert_eq!(edited["status"], "completed");
    assert_eq!(
        call(&root, "read", json!({"area":"memory","project_id":0}))["value"]["text"],
        "Начало\n\nНовый факт 🌲\n\nКонец"
    );
    assert_eq!(
        call(
            &root,
            "edit",
            json!({"project_id":0,"area":"memory","old_text":"нет","new_text":"x","expected_revision":edited["value"]["revision"]})
        )["code"],
        "AGENT_EDIT_AMBIGUOUS"
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn real_process_exit_recovery() {
    for phase in ["pending", "target", "history"] {
        let root = root();
        let executable = std::env::current_exe().unwrap();
        let status = std::process::Command::new(executable)
            .args(["--exact", "agent_files::tests::crash_child", "--nocapture"])
            .env("LES_TEST_AGENT_ROOT", &root)
            .env("LES_TEST_AGENT_PHASE", phase)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));
        let read = call(&root, "read", json!({"area":"memory","project_id":0}));
        assert_eq!(read["value"]["text"], "Durable 🌲", "{phase}: {read}");
        assert!(!root.join("agent/pending.json").exists());
        assert_eq!(
            call(&root, "history", json!({"area":"memory","project_id":0}))["value"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn crash_child() {
    let Ok(root) = std::env::var("LES_TEST_AGENT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("LES_TEST_AGENT_PHASE").unwrap();
    let base = paths::base(&root, true).unwrap().unwrap();
    let _lease = transaction::lease(&base).unwrap();
    transaction::change_with_hook(
        &base,
        "save",
        args("Durable 🌲", "missing").as_object().unwrap(),
        |at| {
            if at == phase {
                std::process::exit(77)
            }
        },
    )
    .unwrap();
}
#[test]
fn external_edit_after_crash_wins() {
    let root = root();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "agent_files::tests::crash_child"])
        .env("LES_TEST_AGENT_ROOT", &root)
        .env("LES_TEST_AGENT_PHASE", "pending")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    fs::write(root.join("agent/MEMORY.md"), "External editor").unwrap();
    assert_eq!(
        call(&root, "read", json!({"area":"memory","project_id":0}))["value"]["text"],
        "External editor"
    );
    assert_eq!(
        call(&root, "history", json!({"area":"memory","project_id":0}))["value"]["items"][0]
            ["status"],
        "conflict"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleted_files_remain_discoverable_and_restore_exact_bytes() {
    let root = root();
    let path = "notes/План 日本語.md";
    let args = json!({"area":"files","project_id":0,"path":path,"text":"\u{feff}Текст 🌲","expected_revision":"missing"});
    let saved = call(&root, "save", args.clone());
    assert_eq!(saved["status"], "completed");
    let deleted = call(
        &root,
        "delete",
        json!({"area":"files","project_id":0,"path":path,"expected_revision":saved["value"]["revision"]}),
    );
    assert_eq!(deleted["value"]["revision"], "missing");
    assert!(!root.join("agent/workspaces/0").join(path).exists());
    assert_eq!(
        call(&root, "list", json!({"project_id":0}))["value"]["files"],
        json!([])
    );
    let listed = call(
        &root,
        "list",
        json!({"project_id":0,"include_deleted":true}),
    );
    assert_eq!(listed["value"]["files"][0]["exists"], false);
    assert_eq!(listed["value"]["files"][0]["path"], path);
    let restored = call(
        &root,
        "restore",
        json!({"area":"files","project_id":0,"path":path,"expected_revision":"missing","history_id":deleted["value"]["history_id"],"side":"before"}),
    );
    assert_eq!(restored["value"]["revision"], saved["value"]["revision"]);
    assert_eq!(
        fs::read(root.join("agent/workspaces/0").join(path)).unwrap(),
        args["text"].as_str().unwrap().as_bytes()
    );
    assert_eq!(
        call(
            &root,
            "list",
            json!({"project_id":0,"include_deleted":true})
        )["value"]["files"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call(
            &root,
            "delete",
            json!({"area":"user","project_id":0,"expected_revision":"missing"})
        )["code"],
        "AGENT_INVALID"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn delete_process_exit_recovers_file_history_and_tombstone() {
    for phase in ["pending", "target", "history"] {
        let root = root();
        let saved = call(
            &root,
            "save",
            json!({"project_id":0,"area":"files","path":"one.md","text":"Restore me 🌲","expected_revision":"missing"}),
        );
        assert_eq!(saved["status"], "completed");
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "agent_files::tests::delete_crash_child"])
            .env("LES_TEST_AGENT_ROOT", &root)
            .env("LES_TEST_AGENT_PHASE", phase)
            .status()
            .unwrap();
        assert_eq!(result.code(), Some(77));
        let listed = call(
            &root,
            "list",
            json!({"project_id":0,"include_deleted":true}),
        );
        assert_eq!(
            listed["value"]["files"][0]["exists"], false,
            "{phase}: {listed}"
        );
        let history = call(
            &root,
            "history",
            json!({"project_id":0,"area":"files","path":"one.md"}),
        );
        assert_eq!(history["value"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(history["value"]["items"][0]["exists"], false);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn delete_crash_child() {
    let Ok(root) = std::env::var("LES_TEST_AGENT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("LES_TEST_AGENT_PHASE").unwrap();
    let base = paths::base(&root, true).unwrap().unwrap();
    let _lease = transaction::lease(&base).unwrap();
    let revision = revision(
        read(&base.join("workspaces/0/one.md"), FILE_LIMIT)
            .unwrap()
            .as_deref(),
    );
    transaction::change_with_hook(
        &base,
        "delete",
        json!({"project_id":0,"area":"files","path":"one.md","expected_revision":revision})
            .as_object()
            .unwrap(),
        |at| {
            if at == phase {
                std::process::exit(77)
            }
        },
    )
    .unwrap();
}

#[test]
fn projection_is_unicode_bounded_and_missing_fields_are_actionable() {
    let root = root();
    call(
        &root,
        "save",
        json!({"project_id":0,"area":"user","text":format!("Коротко 🌲\n\n{}","🌲".repeat(1600)),"expected_revision":"missing"}),
    );
    let projected = call(&root, "projection", json!({"project_id":0}));
    assert_eq!(
        projected["value"]["records"][0]["payload"]["text"],
        "Коротко 🌲"
    );
    assert_eq!(projected["value"]["omitted"], 1);
    let invalid = call(
        &root,
        "save",
        json!({"project_id":0,"area":"memory","text":"a"}),
    );
    assert!(invalid["detail"]
        .as_str()
        .unwrap()
        .contains("expected_revision"));
    assert_eq!(invalid["code"], "AGENT_INVALID");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reader_waits_for_publication_lease() {
    let root = root();
    let base = paths::base(&root, true).unwrap().unwrap();
    let lease = transaction::lease(&base).unwrap();
    let other = root.clone();
    let (sent, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        sent.send(call(&other, "projection", json!({"project_id":0})))
            .unwrap()
    });
    assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
    transaction::change(
        &base,
        "save",
        args("Committed 🌲", "missing").as_object().unwrap(),
    )
    .unwrap();
    drop(lease);
    let projected = received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        projected["value"]["records"][0]["payload"]["text"],
        "Committed 🌲"
    );
    reader.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}
