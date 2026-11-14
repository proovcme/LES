//! Read the running instance's published address, verify ownership, and call its canonical API.
use reqwest::{
    blocking::{Client, Response},
    Method, Url,
};
use serde_json::{json, Value};
use std::{env, fs::File, io::Read, path::PathBuf, time::Duration};

pub type Result<T> = std::result::Result<T, String>;
const OFFLINE: &str = "LES RAG не запущен. Откройте приложение и повторите запрос";
const UNAVAILABLE: &str = "LES RAG не ответил. Проверьте, что приложение запущено";
const CHANGED: &str = "Экземпляр LES RAG изменился. Повторите запрос после запуска";
const INVALID: &str = "LES RAG вернул некорректный ответ";

#[derive(Debug, PartialEq, Eq)]
struct Owner {
    base: Url,
    instance: String,
}

impl Owner {
    fn read(path: &PathBuf) -> Result<Self> {
        let data = File::open(path).map_err(|_| OFFLINE)?;
        let status = read_json(data, 65_536).map_err(|_| OFFLINE)?;
        let instance = status["instance_id"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 256);
        if status["schema"] != "les.light-launcher.v1"
            || status["phase"] != "ready"
            || instance.is_none()
        {
            return Err("LES RAG ещё не готов. Дождитесь запуска приложения".into());
        }
        let base = status["api_url"]
            .as_str()
            .and_then(|url| Url::parse(url).ok())
            .filter(|url| {
                url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
                    && url.port().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
            .ok_or("Адрес API в состоянии LES RAG некорректен")?;
        let base = Url::parse(&format!("http://127.0.0.1:{}", base.port().unwrap()))
            .map_err(|_| INVALID)?;
        Ok(Self {
            base,
            instance: instance.unwrap().into(),
        })
    }
}

pub(super) fn state_root() -> Result<PathBuf> {
    let configured = env::var("LES_LIGHT_STATE").unwrap_or_default();
    let root = if !configured.trim().is_empty() {
        PathBuf::from(configured.trim())
    } else {
        #[cfg(target_os = "macos")]
        let root = env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join("Library/Application Support/LES Light"));
        #[cfg(not(target_os = "macos"))]
        let root = env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| path.join("LES Light"));
        root.ok_or("Укажите LES_LIGHT_STATE — папку данных запущенного LES RAG")?
    };
    if !root.is_absolute() {
        return Err("Укажите абсолютный путь в LES_LIGHT_STATE.".into());
    }
    Ok(root)
}

fn read_json(reader: impl Read, limit: u64) -> Result<Value> {
    let mut data = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut data)
        .map_err(|_| UNAVAILABLE)?;
    if data.len() as u64 > limit {
        return Err("Ответ LES RAG превышает предел размера".into());
    }
    serde_json::from_slice(&data).map_err(|_| INVALID.into())
}

fn decode(response: Response, limit: u64) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        // Keep a safe machine-readable reason rather than replacing every
        // failed read with the same HTTP-status message. Never print raw bodies.
        if let Ok(value) = read_json(response, 65536) {
            if let Some(code) = value["code"].as_str().filter(|s| {
                !s.is_empty()
                    && s.len() <= 100
                    && s.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            }) {
                return Err(format!("{code}: {}", source_failure(code)));
            }
        }
        return Err(match status.as_u16() {
            400 => "Лес отклонил параметры запроса. Проверьте название или поля.".into(),
            401 => "Нет доступа к данным LES RAG. Проверьте ключ MCP-подключения".into(),
            403 => "Лес запретил действие. Проверьте права подключения, разрешённые папки и правила памяти.".into(),
            404 => "Набор, файл или задание не найдено.".into(),
            409 => "Данные изменились. Перечитайте текущую ревизию перед повторной записью.".into(),
            422 => "Проверьте поля запроса, относительный путь и ревизию файла.".into(),
            code => format!("LES RAG вернул отказ {code}. Проверьте состояние приложения"),
        });
    }
    read_json(response, limit)
}

fn source_failure(code: &str) -> &'static str {
    match code {
        "SOURCE_REVISION_CHANGED" | "PDF_REVISION_CHANGED" | "INDEX_CHANGED_DURING_SEARCH" => "Редакция изменилась. Повторите поиск; не объединяйте старые и новые выдержки.",
        "SOURCE_DOCUMENT_NOT_FOUND" | "SOURCE_DOCUMENT_MISSING" => "Исходный документ не найден или отмечен пропавшим.",
        "SOURCE_UNAVAILABLE" | "PDF_READ_FAILED" | "SOURCE_READ_FAILED" => "Не удалось прочитать оригинал. Это не означает, что в нём нет ответа.",
        "PDF_TEXT_LIMIT" => "Укажите max_chars от 1 до 12000 и дочитывайте страницу через next_offset.",
        "PDF_OCR_REQUIRED" => "Страница является сканом. Для чтения оригинала нужен настроенный OCR; индексный фрагмент не заменяет проверку страницы.",
        "PDF_PAGE_OUT_OF_RANGE" => "Номер страницы выходит за границы оригинала. Проверьте page_count.",
        "SOURCE_SECTION_NOT_FOUND" => "В выбранной области индексированных разделов совпадений нет. Это не доказывает отсутствие ответа в полном оригинале.",
        "EMBEDDING_MODEL_NOT_ASSIGNED" => "Назначьте модель поиска в Лесе. Модель ответов для этого не нужна.",
        "SOURCE_INDEX_NOT_READY" => "В выбранных наборах пока нет готовых поисковых фрагментов. Подготовьте документы к поиску; пустой индекс не означает отсутствие ответа в оригиналах.",
        "TOOL_SCHEMA_INVALID" => "Проверьте параметры инструмента по его схеме.",
        "TOOL_SCOPE_VIOLATION" | "CHAT_SCOPE_EMPTY" => "В выбранной области нет доступных источников. Проверьте наборы документов.",
        "CONTEXT_REQUIRED_SECTION_OVERFLOW" => "Целый фрагмент не помещается. Увеличьте max_chars или прочитайте физическую страницу PDF порциями.",
            "SOURCE_CONTINUATION_VIEW_REQUIRED" => "Для продолжения чтения передайте view_sha256 из предыдущей порции вместе с next_offset.",
            "SOURCE_VIEW_CHANGED" => "Изменился снимок прочитанного текста. Начните чтение заново; старые и новые порции нельзя объединять.",
            "SOURCE_READ_LIMIT" | "CONTEXT_SOURCE_LIMIT" => "Для одного чтения найдено слишком много текста. Уточните область поиска или читайте выбранный документ порциями; источники не были молча пропущены.",
            "SOURCE_ATOMIC_UNIT_OVERFLOW" => "Целая строка таблицы или абзац не помещается. Увеличьте max_chars; фрагмент не был обрезан.",
            "OCR_NO_TEXT" => "На странице не удалось распознать текст. Это не подтверждает отсутствие сведений в оригинале.",
            "OCR_UNAVAILABLE" => "Локальный OCR недоступен. Проверьте установленный обработчик распознавания; ответ по скану не подтверждён.",
        "SOURCE_CONTINUATION_REVISION_REQUIRED" => "Для продолжения передайте index_revision из предыдущей порции и повторите те же параметры чтения.",
        _ => "Лес не завершил чтение. Проверьте состояние источника и повторите запрос; не используйте отказ как доказательство.",
    }
}

pub struct Api {
    client: Client,
    path: PathBuf,
    owner: Owner,
    key: String,
}

impl Api {
    pub(super) fn part_image(&self, link: &str) -> Result<Vec<u8>> {
        let url = self.owner.base.join(link).map_err(|_| INVALID)?;
        let pieces = url.path_segments().ok_or(INVALID)?.collect::<Vec<_>>();
        let valid_hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let query = url.query_pairs().collect::<Vec<_>>();
        if url.origin() != self.owner.base.origin()
            || url.fragment().is_some()
            || pieces.len() != 6
            || pieces[..3] != ["api", "documents", "by-id"]
            || pieces[4] != "parts"
            || !valid_hash(pieces[5])
            || query.len() != 1
            || query[0].0 != "source_sha256"
            || !valid_hash(&query[0].1)
        {
            return Err(
                "SOURCE_PART_INVALID: preview must belong to the connected LES instance".into(),
            );
        }
        // Use the normal owner check, authorization and no-redirect client.
        self.get("/api/light/instance")?;
        let mut request = self
            .client
            .get(url.clone())
            .timeout(Duration::from_secs(25));
        if !self.key.is_empty() {
            request = request.header("X-API-Key", &self.key);
        }
        let response = request.send().map_err(|_| UNAVAILABLE)?;
        if !response.status().is_success() {
            decode(response, 65536)?;
            return Err(INVALID.into());
        }
        if response
            .headers()
            .get("content-type")
            .and_then(|s| s.to_str().ok())
            .is_none_or(|s| !s.starts_with("image/png"))
        {
            return Err(INVALID.into());
        }
        let mut bytes = Vec::new();
        response
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| UNAVAILABLE)?;
        if bytes.len() > 4 * 1024 * 1024
            || bytes.len() < 24
            || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || les_core::sha256_hex(&bytes) != pieces[5]
        {
            return Err("SOURCE_PART_CORRUPT: image checksum or size mismatch".into());
        }
        let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        if w == 0 || h == 0 || w as u64 * h as u64 > 25_000_000 {
            return Err("SOURCE_READ_LIMIT: image dimensions".into());
        }
        if Owner::read(&self.path)? != self.owner {
            return Err(CHANGED.into());
        }
        Ok(bytes)
    }
    pub fn source_call(&self, name: &str, args: Value) -> Result<Value> {
        let mut result = self.post(
            "/api/rag/sources/call",
            json!({"tool":name,"args":args}),
            155,
        )?;
        self.source_links(&mut result);
        Ok(result)
    }

    fn source_links(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["url", "parts_url", "preview_url"] {
                    if let Some(link) = map.get_mut(key) {
                        if let Some(path) = link
                            .as_str()
                            .filter(|s| s.starts_with("/api/documents/by-id/"))
                        {
                            if let Ok(url) = self.owner.base.join(path) {
                                *link = json!(url.to_string());
                            }
                        }
                    }
                }
                for item in map.values_mut() {
                    self.source_links(item);
                }
            }
            Value::Array(rows) => {
                for row in rows {
                    self.source_links(row);
                }
            }
            _ => {}
        }
    }
    pub fn connect() -> Result<Self> {
        let path = state_root()?.join("launcher-status.json");
        let owner = Owner::read(&path)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| UNAVAILABLE)?;
        Ok(Self {
            client,
            path,
            owner,
            key: env::var("LES_LIGHT_MCP_API_KEY")
                .unwrap_or_default()
                .trim()
                .into(),
        })
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        seconds: u64,
    ) -> Result<Value> {
        let identity = self
            .client
            .get(
                self.owner
                    .base
                    .join("/api/light/instance")
                    .map_err(|_| INVALID)?,
            )
            .timeout(Duration::from_secs(5))
            .send()
            .map_err(|_| UNAVAILABLE)?;
        let identity = decode(identity, 16_384)?;
        if identity["instance_id"] != self.owner.instance || Owner::read(&self.path)? != self.owner
        {
            return Err(CHANGED.into());
        }
        let mut request = self
            .client
            .request(method, self.owner.base.join(path).map_err(|_| INVALID)?)
            .timeout(Duration::from_secs(seconds));
        if !self.key.is_empty() {
            request = request.header("X-API-Key", &self.key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let result = decode(request.send().map_err(|_| UNAVAILABLE)?, 32 * 1024 * 1024)?;
        if Owner::read(&self.path)? != self.owner {
            return Err(CHANGED.into());
        }
        Ok(result)
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None, 25)
    }

    pub fn get_with_query(&self, path: &str, pairs: &[(&str, &str)]) -> Result<Value> {
        let mut url = self.owner.base.join(path).map_err(|_| INVALID)?;
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        }
        let endpoint = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        self.get(&endpoint)
    }

    pub fn post(&self, path: &str, body: Value, seconds: u64) -> Result<Value> {
        self.request(Method::POST, path, Some(body), seconds)
    }

    pub fn create_chat(&self, question: &str) -> Result<String> {
        self.create_chat_in_project(question, None)
    }

    pub fn create_chat_in_project(
        &self,
        question: &str,
        project_id: Option<u64>,
    ) -> Result<String> {
        let title: String = question.chars().take(80).collect();
        let response = self.request(
            Method::POST,
            "/api/workspace/sessions",
            Some(if let Some(id) = project_id {
                json!({"title":title,"project_id":id})
            } else {
                json!({"title":title})
            }),
            25,
        )?;
        response["session_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or(INVALID.into())
    }

    pub fn ask(&self, sid: &str, question: String, datasets: Vec<String>) -> Result<Value> {
        let mode = if datasets.is_empty() {
            "agent"
        } else {
            "search"
        };
        let mut response = self.request(
            Method::POST,
            "/api/chat",
            Some(json!({
                "session_id":sid,"question":question,"dataset_ids":datasets,"mode":mode,
            })),
            240,
        )?;
        response
            .as_object_mut()
            .ok_or(INVALID)?
            .insert("session_id".into(), json!(sid));
        Ok(response)
    }

    pub fn datasets(&self) -> Result<Value> {
        let result = self.get("/api/documents/datasets?limit=1000")?;
        Ok(dataset_summary(&result))
    }

    pub fn library(&self) -> Result<Value> {
        Ok(dataset_summary(&self.get("/api/light/library")?))
    }

    pub fn require_user_dataset(&self, id: &str) -> Result<()> {
        let result =
            self.get_with_query("/api/light/library", &[("dataset_id", id), ("limit", "1")])?;
        if result["dataset_id"] != id {
            return Err(INVALID.into());
        }
        Ok(())
    }

    pub fn document_links(&self, result: &mut Value) {
        if let Some(docs) = result["documents"].as_array_mut() {
            for row in docs {
                let link = self.document_link(
                    text(&row["id"]),
                    text(&row["dataset_id"]),
                    text(&row["file_name"]),
                    text(&row["file_hash"]),
                );
                row["original_url"] = link.into();
            }
        }
    }

    fn document_link(&self, id: &str, dataset: &str, document: &str, revision: &str) -> String {
        if dataset.is_empty() || document.is_empty() {
            return String::new();
        }
        let mut url = self.owner.base.clone();
        url.path_segments_mut()
            .expect("verified HTTP base")
            .clear()
            .extend([
                "api",
                "documents",
                "by-id",
                if id.is_empty() { "unknown" } else { id },
                "raw",
            ]);
        url.query_pairs_mut()
            .append_pair("dataset_id", dataset)
            .append_pair("doc_name", document);
        if !revision.is_empty() {
            url.query_pairs_mut().append_pair("source_sha256", revision);
        }
        url.to_string()
    }

    pub fn read_clause(&self, dataset: &str, document: &str, number: &str) -> Result<Value> {
        self.require_user_dataset(dataset)?;
        if document.len() > 240
            || !document
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
        {
            return Err("Укажите ID документа из list_documents.".into());
        }
        self.source_call(
            "read_source",
            json!({"dataset_id":dataset,"doc_id":document,"clause":number,"max_chars":16000}),
        )
    }

    pub fn search(&self, query: &str, ids: Option<Vec<String>>, limit: u64) -> Result<Value> {
        if query.trim().is_empty() || query.chars().count() > 4000 || !(1..=20).contains(&limit) {
            return Err("Введите запрос до 4000 символов и limit от 1 до 20".into());
        }
        let mut args = json!({"q":query,"limit":limit});
        if let Some(ids) = ids {
            args["dataset_ids"] = json!(ids);
        }
        self.source_call("search_sources", args)
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn dataset_summary(result: &Value) -> Value {
    let rows: Vec<Value> = array(&result["datasets"])
        .iter()
        .filter(|row| row["dataset_scope"] != "system")
        .map(|row| {
            json!({"id":text(&row["id"]),"name":first_text(&row["display_name"], &row["name"]),
            "documents":count(&row["document_count"]),"declared_chunks":count(&row["chunk_count"])})
        })
        .collect();
    json!({"schema":"les.light.mcp.datasets.v1","datasets":rows})
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
fn first_text<'a>(first: &'a Value, second: &'a Value) -> &'a str {
    first
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(second))
}
fn count(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| text(value).parse().ok())
        .unwrap_or_default()
}
