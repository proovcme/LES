//! The installed MCP stdio protocol endpoint.
use super::{gateway::Result, mcp_tools};
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};

const CURRENT: &str = "2025-11-25";
const VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", CURRENT];
const MAX_MESSAGE: u64 = 1024 * 1024;

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn tool_result(result: Result<Value>) -> Value {
    match result {
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
        }
        Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
    }
}

#[derive(Default)]
enum Phase {
    #[default]
    New,
    Negotiated,
    Ready,
}

fn handle(message: Value, phase: &mut Phase) -> Option<Value> {
    let id = &message["id"];
    let method = message["method"].as_str();
    if message["jsonrpc"] != "2.0"
        || method.is_none()
        || message.get("id").is_some() && !(id.is_string() || id.is_i64() || id.is_u64())
    {
        return Some(error(&Value::Null, -32600, "Invalid Request"));
    }
    let method = method.unwrap();
    if message.get("id").is_none() {
        if method == "notifications/initialized" && matches!(phase, Phase::Negotiated) {
            *phase = Phase::Ready;
        }
        return None;
    }
    let result = match method {
        "initialize" if matches!(phase, Phase::New) => {
            let Some(version) = message["params"]["protocolVersion"].as_str() else {
                return Some(error(id, -32602, "Protocol version required"));
            };
            *phase = Phase::Negotiated;
            json!({"protocolVersion":if VERSIONS.contains(&version) { version } else { CURRENT },
                "capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"LES RAG","version":env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        _ if !matches!(phase, Phase::Ready) => {
            return Some(error(id, -32002, "Server not initialized"))
        }
        "tools/list" => mcp_tools::list(),
        "tools/call" => {
            let params = &message["params"];
            let arguments = &params["arguments"];
            if !arguments.is_null() && !arguments.is_object() {
                return Some(error(id, -32602, "Tool arguments must be an object"));
            }
            let Some(name) = params["name"].as_str() else {
                return Some(error(id, -32602, "Tool name required"));
            };
            tool_result(mcp_tools::call(name, arguments))
        }
        "resources/list" => json!({"resources":[]}),
        "resources/templates/list" => json!({"resourceTemplates":[]}),
        "prompts/list" => json!({"prompts":[]}),
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn write_message(output: &mut impl Write, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, message)?;
    writeln!(output)?;
    output.flush()
}

fn exchange(mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut phase = Phase::New;
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut input)
            .take(MAX_MESSAGE + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        if line.len() as u64 > MAX_MESSAGE {
            return write_message(
                &mut output,
                &error(&Value::Null, -32700, "Message size limit"),
            );
        }
        let response = match serde_json::from_slice(&line) {
            Ok(message) => handle(message, &mut phase),
            Err(_) => Some(error(&Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            write_message(&mut output, &response)?;
        }
    }
}

pub fn serve() -> io::Result<()> {
    exchange(io::stdin().lock(), io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdio_initializes_and_labels_mutations() {
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":"client","method":"tools/list"}),
        ].map(|value| value.to_string()).join("\n");
        let mut output = Vec::new();
        exchange(requests.as_bytes(), &mut output).unwrap();
        let lines: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["error"]["code"], -32002);
        assert_eq!(lines[1]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(lines[2]["id"], "client");
        let tools = lines[2]["result"]["tools"].as_array().unwrap();
        let find = |name| tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(find("list_datasets")["annotations"]["readOnlyHint"], true);
        assert_eq!(find("create_dataset")["annotations"]["readOnlyHint"], false);
        assert_eq!(
            find("workspace_write")["annotations"]["destructiveHint"],
            true
        );
    }
}
