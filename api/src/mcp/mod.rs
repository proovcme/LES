//! Native MCP catalog and clients. Explicitly enabled read-only tools only.
mod http;
mod routes;
pub use routes::{handle, route};
mod stdio;
use les_core::mcp_registry as registry;
use reqwest::Url;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
fn bad(message: &str) -> String {
    message.into()
}
pub(super) fn url(raw: &str) -> std::result::Result<Url, &'static str> {
    let parsed = Url::parse(raw).map_err(|_| "MCP_URL_INVALID")?;
    if raw.len() > 2048
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("MCP_URL_INVALID");
    }
    if parsed.scheme() == "http"
        && !matches!(
            parsed.host_str().unwrap().trim_matches(['[', ']']),
            "localhost" | "127.0.0.1" | "::1"
        )
    {
        return Err("MCP_HTTPS_REQUIRED");
    }
    Ok(parsed)
}
fn catalog<T>(result: std::result::Result<T, les_core::workspace::Failure>) -> Result<T> {
    result.map_err(|v| v.detail)
}
// Avoid exporting an untrusted server instruction or accepting a foreign response id.
pub(super) fn response(
    value: Value,
    expected: &Value,
) -> std::result::Result<Option<Value>, &'static str> {
    if value["jsonrpc"] != "2.0" {
        return Err("MCP_RESPONSE_INVALID");
    }
    if value.get("id").is_none() && value["method"].is_string() {
        return Ok(None);
    }
    if value.get("id") != Some(expected) {
        return Err("MCP_RESPONSE_ID_MISMATCH");
    }
    if value["error"].is_object() {
        return Err("MCP_REMOTE_ERROR");
    }
    if value["method"].is_string() {
        return Err("MCP_UNSUPPORTED_SERVER_REQUEST");
    }
    value
        .get("result")
        .cloned()
        .map(Some)
        .ok_or("MCP_RESPONSE_INVALID")
}
enum Transport {
    Http(http::Http),
    Stdio(stdio::Process),
}
struct Session {
    transport: Transport,
    sequence: u64,
    deadline: Instant,
}
impl Session {
    async fn close(&mut self) {
        if let Transport::Http(http) = &mut self.transport {
            http.close().await;
        }
    }
    async fn connect(item: &Value) -> std::result::Result<Self, &'static str> {
        let transport = if item["transport"] == "stdio" {
            Transport::Stdio(stdio::Process::start(item)?)
        } else {
            Transport::Http(http::Http::new(item["url"].as_str().ok_or("MCP_URL_INVALID")?).await?)
        };
        let mut session = Self {
            transport,
            sequence: 0,
            deadline: Instant::now() + Duration::from_secs(25),
        };
        let initialized=session.rpc("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"LES","version":"0.1.0"}})).await?;
        let version = initialized["protocolVersion"]
            .as_str()
            .filter(|v| matches!(*v, "2024-11-05" | "2025-03-26" | "2025-06-18"))
            .ok_or("MCP_PROTOCOL_UNSUPPORTED")?;
        if let Transport::Http(http) = &mut session.transport {
            http.protocol = Some(version.into());
        }
        session
            .notify(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        Ok(session)
    }
    async fn notify(&mut self, value: Value) -> std::result::Result<(), &'static str> {
        match &mut self.transport {
            Transport::Http(http) => {
                http.exchange(&value, self.deadline).await?;
            }
            Transport::Stdio(process) => process.send(&value)?,
        }
        Ok(())
    }
    async fn rpc(
        &mut self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, &'static str> {
        self.sequence += 1;
        let id = json!(self.sequence);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        match &mut self.transport {
            Transport::Http(http) => http.exchange(&request, self.deadline).await,
            Transport::Stdio(process) => {
                process.send(&request)?;
                for _ in 0..256 {
                    let value = process.receive(self.deadline)?;
                    if value["method"] == "ping" && value["id"].is_number() {
                        process.send(&json!({"jsonrpc":"2.0","id":value["id"],"result":{}}))?;
                        continue;
                    }
                    if let Some(response) = response(value, &id)? {
                        return Ok(response);
                    }
                }
                Err("MCP_RESPONSE_LIMIT")
            }
        }
    }
    async fn tools(&mut self) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor = Value::Null;
        let mut cursors = HashSet::new();
        let mut names = HashSet::new();
        for _ in 0..8 {
            let page = self
                .rpc(
                    "tools/list",
                    if cursor.is_null() {
                        json!({})
                    } else {
                        json!({"cursor":cursor})
                    },
                )
                .await
                .map_err(network)?;
            for tool in page["tools"]
                .as_array()
                .ok_or_else(|| bad("Сервер передал некорректный список инструментов"))?
            {
                if tool.to_string().len() > 32000 {
                    return Err(bad("Описание инструмента MCP слишком большое"));
                }
                let name = tool["name"]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 256)
                    .ok_or_else(|| bad("Некорректное имя инструмента MCP"))?;
                if !names.insert(name.to_string()) {
                    return Err(bad("Сервер вернул повторяющиеся имена инструментов"));
                }
                tools.push(tool.clone());
                if tools.len() > 128 {
                    return Err(bad("Сервер вернул больше 128 инструментов"));
                }
            }
            cursor = page["nextCursor"].clone();
            if cursor.is_null() || cursor == "" {
                return Ok(tools);
            }
            if !cursor.is_string() || !cursors.insert(cursor.to_string()) {
                return Err(bad("Сервер повторяет страницу списка инструментов"));
            }
        }
        Err(bad("Слишком много страниц инструментов MCP"))
    }
}
fn network(code: &str) -> String {
    format!("Сервер MCP не ответил корректно. Проверьте адрес, доступ и состояние сервера ({code})")
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| bad("Некорректный запрос MCP"))
}
async fn perform(request: &Value) -> Result<Value> {
    if request["schema"] != "les.mcp.v1" {
        return Err(bad("Некорректный запрос MCP"));
    }
    let database = PathBuf::from(field(request, "database")?);
    if !database.is_absolute() {
        return Err(bad("Некорректный каталог MCP"));
    }
    let action = field(request, "action")?;
    if action == "list" {
        return Ok(json!({"connections":catalog(registry::list(&database))?}));
    }
    if matches!(action, "save" | "update") {
        let name = field(request, "name")?.trim();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(bad("Введите название подключения длиной до 120 символов"));
        }
        let transport = request["transport"].as_str().unwrap_or("http");
        let (raw, config) = match transport {
            "http" => {
                let raw = field(request, "url")?.trim();
                url(raw).map_err(|code|if code=="MCP_HTTPS_REQUIRED"{bad("Для удалённого сервера используйте HTTPS")}else{bad("Укажите адрес HTTP/HTTPS без пароля, параметров и фрагмента, например http://localhost:8000/mcp")})?;
                (raw.into(), json!({"transport":"http"}))
            }
            "stdio" => {
                let command = Path::new(field(request, "command")?.trim());
                if !command.is_absolute()
                    || !command.is_file()
                    || cfg!(windows)
                        && command
                            .extension()
                            .is_none_or(|v| !v.eq_ignore_ascii_case("exe"))
                {
                    return Err(bad("Укажите полный путь к существующей программе MCP (.exe). Командная строка оболочки не поддерживается"));
                }
                let arguments = request.get("args").cloned().unwrap_or(json!([]));
                if !arguments.as_array().is_some_and(|v| {
                    v.len() <= 32
                        && v.iter().all(|a| {
                            a.as_str()
                                .is_some_and(|s| s.chars().count() <= 4096 && !s.contains('\0'))
                        })
                }) {
                    return Err(bad("Допустимо до 32 аргументов, каждый до 4096 символов"));
                }
                let command = std::path::absolute(command)
                    .map_err(|_| bad("Укажите полный путь к программе"))?;
                let config = json!({"transport":"stdio","command":command,"args":arguments});
                (registry::stdio_identity(&config), config)
            }
            _ => return Err(bad("Выберите HTTP или локальную программу")),
        };
        return if action == "update" {
            catalog(registry::update(
                &database,
                field(request, "connection_id")?,
                name,
                &raw,
                &config,
            ))
        } else {
            catalog(registry::add(&database, name, &raw, &config))
        };
    }
    let id = field(request, "connection_id")?;
    if action == "remove" {
        catalog(registry::remove(&database, id))?;
        return Ok(json!({"status":"deleted"}));
    }
    let mut item = catalog(registry::get(&database, id))?;
    if item["transport"] == "stdio" {
        item["cwd"] = request["working_directory"].clone();
    }
    if action == "enable" && request["names"] == json!([]) {
        return catalog(registry::enable(&database, id, &[], &item));
    }
    let mut session = Session::connect(&item).await.map_err(network)?;
    let outcome = async { match action {
        "discover" => Ok(json!({"tools":session.tools().await?})),
        "enable" => {
            let names = request["names"]
                .as_array()
                .filter(|v| v.len() <= 32)
                .ok_or_else(|| bad("Выберите до 32 разных инструментов"))?;
            let mut seen = HashSet::new();
            if !names
                .iter()
                .all(|v| v.as_str().is_some_and(|s| seen.insert(s)))
            {
                return Err(bad("Выберите до 32 разных инструментов"));
            }
            let discovered = session.tools().await?;
            let mut selected = Vec::new();
            for tool in discovered.iter().filter(|tool|names.contains(&tool["name"])) {
                if tool["annotations"]["readOnlyHint"] != true {
                    return Err(bad("Пока доступны только инструменты, объявленные сервером как чтение"));
                }
                catalog(les_core::tool_registry::local_schema(&tool["inputSchema"]))?;
                selected.push(tool.clone());
            }
            if selected.len()!=names.len(){return Err(bad("Список инструментов изменился. Проверьте подключение ещё раз"));}
            catalog(registry::enable(&database, id, &selected, &item))
        }
        "call" => {
            let expected = &request["expected"];
            if !item["tools"]
                .as_array()
                .is_some_and(|v| v.contains(expected))
            {
                return Err(bad("Инструмент отключён. Обновите профиль"));
            }
            let discovered = session.tools().await?;
            if !discovered.contains(expected) {
                return Err(bad("Описание инструмента изменилось. Проверьте подключение MCP и включите инструмент снова"));
            }
            let arguments = &request["arguments"];
            if !arguments.is_object() {
                return Err(bad("Аргументы инструмента должны быть объектом"));
            }
            catalog(les_core::tool_registry::local_schema(
                &expected["inputSchema"],
            ))?;
            // Validate again in the native registry against the exact enabled contract.
            let local = format!(
                "mcp_{}",
                &les_core::sha256_hex(format!("{id}:{}", field(expected, "name")?).as_bytes())
                    [..32]
            );
            let checked = les_core::tool_registry::execute(
                &json!({"schema":"les.tool-registry.v1","database":database,"action":"validate","name":local,"arguments":arguments}),
            );
            if checked["status"] != "completed" || checked["value"]["valid"] != true {
                return Err(bad("Некорректные аргументы инструмента MCP"));
            }
            let current = catalog(registry::get(&database, id))?;
            let mut current=current;
            if current["transport"]=="stdio" { current["cwd"]=request["working_directory"].clone(); }
            if current != item {
                return Err(bad(
                    "Подключение MCP изменилось во время проверки. Повторите запрос",
                ));
            }
            session
                .rpc(
                    "tools/call",
                    json!({"name":expected["name"],"arguments":arguments}),
                )
                .await
                .map_err(network)
        }
        _ => Err(bad("Неизвестная операция MCP")),
    } }.await;
    session.close().await;
    outcome
}
pub async fn execute(request: &Value) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(28), perform(request)).await;
    match result {
        Ok(Ok(value)) => json!({"ok":true,"value":value}),
        Ok(Err(message)) => {
            json!({"ok":false,"error":{"code":"MCP_REQUEST_FAILED","message":message}})
        }
        Err(_) => {
            json!({"ok":false,"error":{"code":"MCP_TIMEOUT","message":network("MCP_TIMEOUT")}})
        }
    }
}

pub fn serve() -> std::result::Result<(), &'static str> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "MCP_RUNTIME_FAILED")?;
    let mut input = std::io::BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let count = (&mut input)
            .take(8 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "MCP_INPUT_FAILED")?;
        if count == 0 {
            return Ok(());
        }
        if bytes.len() > 8 * 1024 * 1024 || !bytes.ends_with(b"\n") {
            return Err("MCP_INPUT_LIMIT");
        }
        let request: Value = serde_json::from_slice(&bytes).map_err(|_| "MCP_REQUEST_INVALID")?;
        let value = runtime.block_on(execute(&request["input"]));
        let response =
            json!({"request_id":request["request_id"],"result":{"status":"ok","result":value}});
        writeln!(output, "{response}")
            .and_then(|_| output.flush())
            .map_err(|_| "MCP_OUTPUT_FAILED")?;
    }
}
