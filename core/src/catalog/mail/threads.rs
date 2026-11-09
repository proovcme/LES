use super::{db, first, params, registry, rows, safe_id, sql, Result, INVALID};
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::LazyLock,
};

static PREFIX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^\s*((re|fw|fwd|ответ|пересл)\s*(\[\d+\])?\s*:\s*)+").unwrap()
});

fn subject_key(subject: &str) -> String {
    PREFIX
        .replace(subject, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn rebuild_threads(state: &Path, account: Option<&str>) -> Result<Value> {
    if account.is_some_and(|id| !safe_id(id)) {
        return Err(INVALID);
    }
    let mut conn = registry(state)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let all = rows(
        &tx,
        "SELECT id,subject,raw_path FROM mail_messages WHERE (?1 IS NULL OR account_id=?1)",
        [account],
    )
    .map_err(db)?;
    let mut count = 0;
    {
        let mut update = tx
            .prepare("UPDATE mail_messages SET thread_key=?2 WHERE id=?1")
            .map_err(sql)?;
        for row in all {
            let id = row["id"].as_str().ok_or(INVALID)?;
            let subject = subject_key(row["subject"].as_str().unwrap_or_default());
            let path = row["raw_path"].as_str().filter(|s| !s.is_empty());
            let (prefix, value) = if !subject.is_empty() {
                ("subject", subject.as_str())
            } else if let Some(path) = path {
                ("file", path)
            } else {
                ("msg", id)
            };
            let key = format!("{prefix}_{}", &crate::sha256_hex(value.as_bytes())[..12]);
            count += update.execute(params![id, key]).map_err(sql)?;
        }
    }
    tx.commit().map_err(sql)?;
    Ok(json!({"status":"ok","updated_messages":count}))
}

pub fn threads(state: &Path, query: &Value) -> Result<Value> {
    let field = |name: &str| {
        query[name]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let account = field("account_id");
    if account.is_some_and(|id| !safe_id(id)) {
        return Err(INVALID);
    }
    let limit = field("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 500);
    let max_files = field("max_files")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2000)
        .clamp(1, 10000);
    let conn = registry(state)?;
    let mut result = json!({"component":"Е.Ж.И.К.","limit":limit});
    if let Some(id) = account {
        let row = first(
            &conn,
            "SELECT id,dataset_id,dataset_name FROM mail_accounts WHERE id=?1",
            [id],
        )
        .map_err(db)?;
        if row.is_null() {
            return Err("MAIL_ACCOUNT_NOT_FOUND");
        }
        result["account_id"] = row["id"].clone();
        result["dataset_id"] = row["dataset_id"].clone();
        result["dataset_name"] = row["dataset_name"].clone();
    }
    let all = rows(&conn, "SELECT id,account_id,subject,sender,recipients_json,received_at AS date,thread_key,rag_doc_id
        FROM mail_messages WHERE (?1 IS NULL OR account_id=?1) ORDER BY received_at DESC,id LIMIT ?2",
        params![account,max_files as i64]).map_err(db)?;
    let q = field("q").unwrap_or_default().to_lowercase();
    let participant = field("participant").unwrap_or_default().to_lowercase();
    let mut groups = BTreeMap::<(String, String), Vec<Value>>::new();
    let mut participants = BTreeSet::new();
    let mut count = 0;
    for row in all {
        let subject = row["subject"].as_str().unwrap_or_default();
        let sender = row["sender"].as_str().unwrap_or_default();
        let recipients: Vec<String> =
            serde_json::from_str(row["recipients_json"].as_str().unwrap_or("[]"))
                .unwrap_or_default();
        let people = std::iter::once(sender).chain(recipients.iter().map(String::as_str));
        let matches = |needle: &str| {
            needle.is_empty() || people.clone().any(|s| s.to_lowercase().contains(needle))
        };
        if !matches(&participant) || !(matches(&q) || subject.to_lowercase().contains(&q)) {
            continue;
        }
        participants.extend(people.filter(|p| !p.is_empty()).map(str::to_owned));
        let key = (
            row["account_id"].as_str().unwrap_or_default().to_owned(),
            row["thread_key"].as_str().unwrap_or_default().to_owned(),
        );
        groups.entry(key).or_default().push(row);
        count += 1;
    }
    let mut summaries = Vec::with_capacity(groups.len());
    for ((account_id, key), group) in groups {
        let latest = &group[0];
        let mut people = BTreeSet::new();
        for message in &group {
            if let Some(sender) = message["sender"].as_str().filter(|p| !p.is_empty()) {
                people.insert(sender.to_owned());
            }
            if let Some(raw) = message["recipients_json"].as_str() {
                if let Ok(values) = serde_json::from_str::<Vec<String>>(raw) {
                    people.extend(values);
                }
            }
        }
        summaries.push(json!({"thread_key":key,"thread_root":key,"account_id":account_id,
            "subject":latest["subject"],"message_count":group.len(),"participants":people,
            "first_date":group.last().map(|v| &v["date"]),"last_date":latest["date"],"latest":latest}));
    }
    summaries.sort_by(|a, b| b["last_date"].as_str().cmp(&a["last_date"].as_str()));
    result["total_threads"] = json!(summaries.len());
    result["total_messages"] = json!(count);
    result["participants"] = json!(participants);
    summaries.truncate(limit);
    result["threads"] = json!(summaries);
    Ok(result)
}
