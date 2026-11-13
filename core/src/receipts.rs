//! Receipts report native commits and failed attempts; model prose is never evidence.
use serde_json::{json, Map, Value};
use std::collections::HashSet;
type Result<T> = std::result::Result<T, &'static str>;
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.),
    }
}
fn ledger(r: &Value) -> Result<Value> {
    let state = &r["state"];
    let mut counts = Map::new();
    for name in [
        "total",
        "pending",
        "reviewed",
        "needs_review",
        "excluded",
        "needs_recheck",
        "blocked_units",
    ] {
        let count = state
            .get(name)
            .map_or(Ok(0), |v| v.as_u64().ok_or("EXECUTION_COUNTS_INVALID"))?;
        counts.insert(name.into(), json!(count));
    }
    let count = |name: &str| counts[name].as_u64().unwrap();
    let sum = [
        "pending",
        "reviewed",
        "needs_review",
        "excluded",
        "needs_recheck",
    ]
    .iter()
    .try_fold(0u64, |sum, name| sum.checked_add(count(name)))
    .ok_or("EXECUTION_COUNTS_INVALID")?;
    if sum != count("total") {
        return Err("EXECUTION_COUNTS_INVALID");
    }
    let status = if truth(&state["verification_unavailable"]) {
        "unavailable"
    } else if count("needs_recheck") > 0 || count("blocked_units") > 0 {
        "needs_recheck"
    } else if count("pending") > 0 || count("total") == 0 {
        "incomplete"
    } else if count("needs_review") > 0 {
        "needs_attention"
    } else {
        "recorded"
    };
    let saved = count("reviewed") + count("needs_review") + count("excluded");
    let subject = if r["kind"] == "table_document" {
        "строк"
    } else {
        "разделов"
    };
    // Preserve count-only historical receipts. Native review provides causes.
    let diagnosed = state["source_errors"]
        .as_object()
        .is_some_and(|errors| !errors.is_empty());
    let source_changed = state["source_errors"].as_object().is_some_and(|errors| {
        errors
            .values()
            .any(|code| code == "REVIEW_ORIGINAL_CHANGED" || code == "INDEX_CHANGED_DURING_SEARCH")
    });
    let notice=match status {
        "unavailable"=>"Не удалось подтвердить состояние задания. Выполнение действий не подтверждено.".into(),
        "needs_recheck" if count("blocked_units") > 0 && diagnosed && !source_changed=>"Не удалось проверить доступность источника. Требуется перепроверка после восстановления доступа. Сохранённые решения остаются в журнале; их актуальность сейчас не подтверждена.".into(),
        "needs_recheck"=>"Источник или зависимые выводы изменились. Требуется перепроверка; прежний текст не подтверждает актуальность.".into(),
        "incomplete"=>format!("Проверка не завершена: в журнале сохранены решения для {saved} из {} {subject}. Текст модели не подтверждает выполнение остальных действий.",count("total")),
        "needs_attention"=>format!("Решения записаны, но {} {subject} требуют проверки пользователем.",count("needs_review")),
        _=>format!("В журнале сохранены решения для всех {} {subject}. Это подтверждает запись, а не правильность выводов модели.",count("total")),
    };
    if state.get("task_id").is_none() || state.get("revision").is_none() {
        return Err("EXECUTION_REQUEST_INVALID");
    }
    let blocked = count("blocked_units") > 0 && diagnosed;
    let mut receipt = json!({"schema":"les.execution-receipt.v1","kind":r["kind"],"task_id":state["task_id"],"revision":state["revision"],"status":status,"counts":counts,"saved":saved,"accepted":false,"notice":notice});
    if blocked {
        receipt["source_check"] = json!(if source_changed {
            "changed"
        } else {
            "unavailable"
        });
        receipt["source_errors"] = state.get("source_errors").cloned().unwrap_or(json!({}));
    }
    Ok(receipt)
}
fn local(r: &Value) -> Value {
    let p = &r["payload"];
    let result = &p["result"];
    let recorded = p["status"] == "ok"
        && result["persisted"] == true
        && result["revision"].as_str().is_some_and(|s| !s.is_empty());
    let path = result["path"].as_str().unwrap_or("");
    let label = if path == "USER.md" {
        "О вас"
    } else if path.ends_with("MEMORY.md") {
        "Память Леса"
    } else if path.is_empty() {
        "Рабочие файлы"
    } else {
        path.splitn(3, '/').last().unwrap_or(path)
    };
    let notice = if recorded {
        if p["operation"] == "delete" {
            format!("Убрано в историю: {label}.")
        } else {
            format!("Сохранено: {label}.")
        }
    } else {
        "Одна из попыток изменения памяти или рабочего файла не выполнена. Проверьте ошибку инструмента и повторите при необходимости.".into()
    };
    json!({"schema":"les.execution-receipt.v1","kind":"agent_file","task_id":path,"revision":result.get("revision").cloned().unwrap_or(json!("")),"status":if recorded {"recorded"} else {"incomplete"},"accepted":false,"history_id":result["history_id"],"operation":p["operation"],"changed":result["changed"]==true,"notice":notice})
}
fn summary(r: &Value) -> Result<Value> {
    let tasks = r["receipts"]
        .as_array()
        .ok_or("EXECUTION_REQUEST_INVALID")?
        .iter()
        .filter(|v| !v.is_null())
        .cloned()
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        return Ok(Value::Null);
    }
    let incomplete = tasks.iter().any(|t| t["status"] != "recorded");
    let mut seen = HashSet::new();
    let mut notices = Vec::new();
    // Preserve every task, including failures. Display actual commits before failed attempts.
    for recorded in [true, false] {
        for task in tasks
            .iter()
            .filter(|t| (t["status"] == "recorded") == recorded)
        {
            let notice = task["notice"].as_str().ok_or("EXECUTION_REQUEST_INVALID")?;
            if seen.insert(notice) {
                notices.push(notice);
            }
        }
    }
    Ok(
        json!({"schema":"les.execution-summary.v1","status":if incomplete {"incomplete"} else {"recorded"},"tasks":tasks,"notice":notices.join("\n"),"accepted":false}),
    )
}
pub fn execute(r: &Value) -> Result<Value> {
    match r["action"].as_str() {
        Some("ledger_receipt") => Ok(json!({"receipt":ledger(r)?})),
        Some("local_file_receipt") => Ok(json!({"receipt":local(r)})),
        Some("receipt_summary") => Ok(json!({"summary":summary(r)?})),
        Some("no_local_changes") => Ok(
            json!({"summary":{"schema":"les.execution-summary.v1","status":"no_changes","tasks":[],"accepted":false,"notice":"Изменений в памяти и рабочих файлах нет."}}),
        ),
        _ => Err("EXECUTION_REQUEST_INVALID"),
    }
}
