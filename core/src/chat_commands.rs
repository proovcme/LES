//! Explicit slash commands only. Ordinary prose always goes through the model.
use serde_json::{json, Value};
use std::sync::LazyLock;
static COMMANDS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../config/chat-commands.json"))
        .expect("versioned chat commands")
});
fn names(row: &Value) -> Vec<&str> {
    let mut result = vec![row["cmd"].as_str().unwrap()];
    result.extend(
        row["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap()),
    );
    result
}
pub fn list() -> Value {
    json!(COMMANDS
        .iter()
        .map(|c| json!({"cmd":c["cmd"],"title":c["title"],"desc":c["desc"]}))
        .collect::<Vec<_>>())
}
pub fn handle(question: &str) -> Value {
    let question = question.trim();
    if !question.starts_with('/') {
        return Value::Null;
    }
    let token = question
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let entry = COMMANDS
        .iter()
        .find(|r| names(r).contains(&token.as_str()))
        .or_else(|| {
            COMMANDS
                .iter()
                .flat_map(|r| names(r).into_iter().map(move |n| (r, n)))
                .filter(|(_, n)| token.starts_with(n))
                .max_by_key(|(_, n)| n.len())
                .map(|(r, _)| r)
        });
    let Some(entry) = entry else {
        return json!({"answer":format!("Неизвестная команда «{token}». Набери /команды — покажу список."),"command":{"action":"unknown"}});
    };
    match entry["kind"].as_str().unwrap() {
        "rewrite" => json!({"rewrite":entry["rewrite"]}),
        "help" => {
            let mut lines =
                vec!["Команды ЛЕС (можно набрать в чате или выбрать в «/»-меню):".to_owned()];
            for row in COMMANDS.iter() {
                let aliases = row["aliases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "  {}{} — {}",
                    row["cmd"].as_str().unwrap(),
                    if aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" · {aliases}")
                    },
                    row["desc"].as_str().unwrap()
                ));
            }
            json!({"answer":lines.join("\n"),"command":{"action":"help","commands":list()}})
        }
        "info" => {
            json!({"answer":"MCP подключается в разделе «Настройки → Профили → Внешние инструменты». Добавьте адрес сервера, проверьте список и разрешите нужные инструменты, затем выберите их в профиле. Поддерживаются HTTP и локальные программы MCP, инструменты чтения. Вход OAuth, ключи и действия с изменением данных пока недоступны.","command":{"action":"mcp_info","transports":["streamable_http","stdio"]}})
        }
        _ => Value::Null,
    }
}
pub fn execute(request: &Value) -> Result<Value, &'static str> {
    match request["action"].as_str() {
        Some("list") => Ok(list()),
        Some("handle") => Ok(handle(
            request["question"].as_str().ok_or("CHAT_REQUEST_INVALID")?,
        )),
        _ => Err("CHAT_REQUEST_INVALID"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_never_route_plain_text_or_unknown_actions() {
        assert!(handle("Какие команды доступны?").is_null());
        assert_eq!(handle(" /HELP ")["command"]["action"], "help");
        assert_eq!(handle("/командыответь")["command"]["action"], "help");
        assert_eq!(handle("/неизвестно")["command"]["action"], "unknown");
        assert_eq!(handle("/ТЭП")["rewrite"], "дай сводку проекта");
    }
}
