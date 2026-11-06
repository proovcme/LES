//! Terminal entry point for the same LES profile used by the desktop and MCP.
mod gateway;
mod lifecycle;
mod mcp;
mod mcp_tools;

use gateway::Api;
use serde_json::Value;
use std::io::{self, IsTerminal, Read, Write};

const HELP: &str = "ЛЕС: локальное управление, чат и поиск\n\
    les start | stop | restart | status | logs\n\
    les datasets | projects | mcp\n\
    les search \"запрос\" [--dataset ID ...]\n\
    les ask \"вопрос\" [--chat ID] [--dataset ID ...]\n\
    les tools | tool ИМЯ [JSON]; JSON можно передать через stdin";

#[derive(Default)]
struct Question {
    text: String,
    datasets: Vec<String>,
    chat: Option<String>,
}

fn parse_question(args: &[String], ask: bool) -> Result<Question, &'static str> {
    let mut result = Question::default();
    let mut text = None;
    let mut positional = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if !positional && arg == "--" {
            positional = true;
        } else if !positional && arg.starts_with('-') {
            let (flag, value) = arg.split_once('=').unwrap_or((arg, ""));
            if flag != "--dataset" && !(ask && flag == "--chat") {
                return Err("Неизвестный параметр команды Леса");
            }
            let value = if arg.contains('=') {
                value
            } else {
                args.next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or("Укажите значение параметра")?
            };
            if value.is_empty() {
                return Err("Укажите значение параметра");
            }
            if flag == "--dataset" {
                result.datasets.push(value.to_owned());
            } else {
                result.chat = Some(value.to_owned());
            }
        } else if text.replace(arg.clone()).is_some() {
            return Err("Передайте текст одним аргументом в кавычках");
        }
    }
    result.text = text
        .filter(|value| !value.trim().is_empty())
        .ok_or("Укажите текст запроса")?;
    Ok(result)
}

fn execute(command: &str, question: Option<Question>) -> gateway::Result<Value> {
    let api = Api::connect()?;
    match (command, question) {
        ("projects", _) => api.get("/api/projects"),
        ("datasets", _) => api.datasets(),
        ("search", Some(question)) => api.search(
            &question.text,
            (!question.datasets.is_empty()).then_some(question.datasets),
            6,
        ),
        ("ask", Some(question)) => {
            let sid = match question.chat {
                Some(id) => id,
                None => api.create_chat(&question.text)?,
            };
            api.ask(&sid, question.text, question.datasets)
        }
        _ => unreachable!("validated command"),
    }
}

fn tool(name: &str, payload: Option<&String>) -> gateway::Result<Value> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let mut input = Vec::new();
    if let Some(raw) = payload {
        input.extend_from_slice(raw.as_bytes());
    } else if !io::stdin().is_terminal() {
        io::stdin()
            .lock()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut input)
            .map_err(|_| "Не удалось прочитать аргументы инструмента.")?;
    }
    if input.len() as u64 > MAX_BYTES {
        return Err("Аргументы инструмента превышают 1 МиБ.".into());
    }
    let args = if input.iter().all(u8::is_ascii_whitespace) {
        serde_json::json!({})
    } else {
        serde_json::from_slice::<Value>(&input)
            .ok()
            .filter(Value::is_object)
            .ok_or("Передайте объект JSON в UTF-8.")?
    };
    mcp_tools::call(name, &args)
}

pub fn run(args: &[String]) -> i32 {
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{HELP}");
        return 2;
    };
    if matches!(command.as_str(), "--help" | "-h")
        || matches!(rest, [arg] if matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("{HELP}");
        return 0;
    }
    let result = match (command.as_str(), rest) {
        ("ask" | "search", rest) => {
            let question = match parse_question(rest, command == "ask") {
                Ok(question) => question,
                Err(error) => {
                    eprintln!("{error}");
                    return 2;
                }
            };
            execute(command, Some(question))
        }
        ("mcp", []) => return if mcp::serve().is_ok() { 0 } else { 1 },
        ("status", []) => lifecycle::status(),
        ("logs", []) => lifecycle::logs(),
        ("start", []) => lifecycle::start(),
        ("stop", []) => lifecycle::stop(),
        ("restart", []) => lifecycle::stop().and_then(|_| lifecycle::start()),
        ("projects" | "datasets", []) => execute(command, None),
        ("tools", []) => Ok(mcp_tools::list()),
        ("tool", [name]) => tool(name, None),
        ("tool", [name, payload]) => tool(name, Some(payload)),
        _ => {
            eprintln!("{HELP}");
            return 2;
        }
    };
    match result {
        Ok(result) => {
            let mut output = io::stdout().lock();
            if serde_json::to_writer_pretty(&mut output, &result).is_ok()
                && writeln!(output).is_ok()
                && output.flush().is_ok()
            {
                0
            } else {
                1
            }
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_preserves_unicode_and_selection() {
        let input = [
            "--chat=prev",
            "--dataset",
            "a",
            "Где файл #1 日本語?",
            "--dataset=b",
        ];
        let args = input.map(str::to_owned);
        let parsed = parse_question(&args, true).unwrap();
        assert_eq!(parsed.chat.as_deref(), Some("prev"));
        assert_eq!(parsed.text, input[3]);
        assert_eq!(parsed.datasets, ["a", "b"]);
        assert!(parse_question(&args, false).is_err());
        assert!(parse_question(&["--dataset".into()], true).is_err());
    }
}
