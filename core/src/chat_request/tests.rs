use super::*;
use rusqlite::Connection;
fn database() -> std::path::PathBuf {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).unwrap();
    std::env::temp_dir().join(format!("les-chat-{}.sqlite", crate::sha256_hex(&random)))
}
#[test]
fn clause_numbers_match_headings_without_matching_other_numbers_or_body_text() {
    for heading in [
        "15.3 Требования",
        "### 15.3. Требования",
        "Пункт 15.3 Требования",
    ] {
        assert!(clause_heading(heading, "15.3").unwrap());
    }
    for heading in [
        "115.3 Требования",
        "15.3.1 Другое",
        "Текст про 15.3",
        "15.30 Другое",
    ] {
        assert!(!clause_heading(heading, "15.3").unwrap());
    }
    assert!(clause_heading("15.3 Требования", "15.3 OR *").is_err());
}
#[test]
fn canonical_requests_ignore_unknowns_but_never_coerce_authority_fields() {
    let a = validate(&json!({"question":" Лес 日本語 ","private_unused":"ignored"})).unwrap();
    let b = validate(
        &json!({"question":"Лес 日本語","session_id":null,"apply_profile_revision":false}),
    )
    .unwrap();
    assert_eq!(a, b);
    for request in [
        json!({"question":"x","dataset_ids":"all"}),
        json!({"question":"x","project_id":"1"}),
        json!({"question":"x","apply_profile_revision":1}),
    ] {
        assert!(validate(&request).is_err());
    }
}
#[test]
fn receipt_is_single_owner_under_race_and_failed_finish_rolls_back_both_rows() {
    let db = database();
    journal::recover(&db).unwrap();
    let request = validate(&json!({"question":"x"})).unwrap();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            let r = request.clone();
            std::thread::spawn(move || journal::begin(&db, &r, Some("request-1")).unwrap())
        })
        .collect();
    let values: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(values.iter().filter(|v| v["created"] == true).count(), 1);
    let saved = &values[0];
    let id = saved["id"].as_str().unwrap();
    let history = saved["history_id"].as_i64().unwrap();
    assert!(journal::finish(
        &db,
        id,
        history,
        &json!({}),
        &json!({"answer":"cannot commit"})
    )
    .is_err());
    assert_eq!(
        journal::begin(&db, &request, Some("request-1")).unwrap()["status"],
        "running"
    );
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT crag_status FROM chat_history WHERE id=?",
            [history],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "IN_PROGRESS"
    );
    assert_eq!(
        journal::begin(
            &db,
            &validate(&json!({"question":"other"})).unwrap(),
            Some("request-1")
        )
        .unwrap_err(),
        "IDEMPOTENCY_KEY_REUSED"
    );
    drop(conn);
    let _ = std::fs::remove_file(db);
}
#[test]
fn recover_preserves_partial_text_and_never_interrupts_another_coordinator() {
    let db = database();
    journal::recover(&db).unwrap();
    let request = validate(&json!({"question":"x","session_id":"s"})).unwrap();
    let saved = journal::begin(&db, &request, Some("one")).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE chat_history SET answer='Прочитано 日本語' WHERE id=?",
        [saved["history_id"].as_i64().unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chat_history(question,crag_status) VALUES ('legacy-active','IN_PROGRESS')",
        [],
    )
    .unwrap();
    journal::recover(&db).unwrap();
    journal::recover(&db).unwrap();
    assert_eq!(
        journal::begin(&db, &request, Some("one")).unwrap()["status"],
        "interrupted"
    );
    let (answer, status, artifact): (String, String, String) = conn
        .query_row(
            "SELECT answer,crag_status,artifact_json FROM chat_history WHERE id=?",
            [saved["history_id"].as_i64().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (answer.as_str(), status.as_str()),
        ("Прочитано 日本語", "INTERRUPTED")
    );
    assert!(artifact.contains("CHAT_INTERRUPTED"));
    assert_eq!(
        conn.query_row(
            "SELECT crag_status FROM chat_history WHERE question='legacy-active'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "IN_PROGRESS"
    );
    drop(conn);
    let _ = std::fs::remove_file(db);
}
#[test]
fn checkpoint_cannot_append_or_clear_after_the_turn_is_terminal() {
    let db = database();
    journal::recover(&db).unwrap();
    let request = validate(&json!({"question":"x"})).unwrap();
    let receipt = journal::begin(&db, &request, None).unwrap();
    let id = receipt["id"].as_str().unwrap();
    let history = receipt["history_id"].as_i64().unwrap();
    journal::checkpoint(&db, id, history, "token", "Сохранено").unwrap();
    journal::interrupt(
        &db,
        id,
        history,
        Some(&json!({"code":"PDF_REVISION_CHANGED","detail":"Редакция изменилась"})),
    )
    .unwrap();
    assert_eq!(
        journal::checkpoint(&db, id, history, "token", "Дополнение").unwrap_err(),
        "CHAT_REQUEST_STATE_CHANGED"
    );
    assert_eq!(
        journal::checkpoint(&db, id, history, "reset", "").unwrap_err(),
        "CHAT_REQUEST_STATE_CHANGED"
    );
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT answer FROM chat_history WHERE id=?",
            [history],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Сохранено"
    );
    drop(conn);
    let _ = std::fs::remove_file(db);
}
#[test]
fn explicit_scope_intersects_visible_inventory_and_does_not_follow_question_text() {
    let datasets = vec![
        json!({"id":"a","name":"Первый"}),
        json!({"id":"b","name":"Второй"}),
    ];
    let none = validate(&json!({"question":"прочитай все документы из второго"})).unwrap();
    assert_eq!(
        resolve_scope(&none, &datasets, &[]).unwrap()["resolved_dataset_ids"],
        json!([])
    );
    let request =
        validate(&json!({"question":"x","scope":{"scope_type":"projects","project_ids":[1]}}))
            .unwrap();
    let scope = resolve_scope(
        &request,
        &datasets,
        &[json!({"id":1,"dataset_ids":["a","missing"]})],
    )
    .unwrap();
    assert_eq!(scope["resolved_dataset_ids"], json!(["a"]));
    assert_eq!(scope["warnings"], json!(["dataset_not_found"]));
}
