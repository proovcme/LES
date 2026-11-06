//! Read the running instance's published address, verify ownership, and call its canonical API.
use reqwest::{
    blocking::{Client, Response},
    Method, Url,
};
use serde_json::{json, Value};
use std::{collections::HashSet, env, fs::File, io::Read, path::PathBuf, time::Duration};

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

pub struct Api {
    client: Client,
    path: PathBuf,
    owner: Owner,
    key: String,
}

impl Api {
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
                    "",
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
                if revision.is_empty() { "viewer" } else { "raw" },
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
        if document.len() > 240 || !document.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b)) {
            return Err("Укажите ID документа из list_documents.".into());
        }
        let mut result = self.get_with_query(
            &format!("/api/documents/by-id/{document}/clause"),
            &[("dataset_id", dataset), ("number", number)],
        )?;
        if let Some(sources) = result["sources"].as_array_mut() {
            for source in sources {
                source["original_url"] = self.document_link(
                    document, dataset, text(&source["doc_name"]),
                    text(&source["locator"]["source_sha256"]),
                ).into();
            }
        }
        Ok(result)
    }

    pub fn search(&self, query: &str, ids: Option<Vec<String>>, limit: u64) -> Result<Value> {
        self.search_catalog(query, ids, limit, false)
    }

    pub fn search_library(
        &self,
        query: &str,
        ids: Option<Vec<String>>,
        limit: u64,
    ) -> Result<Value> {
        self.search_catalog(query, ids, limit, true)
    }

    fn search_catalog(
        &self,
        query: &str,
        ids: Option<Vec<String>>,
        limit: u64,
        native: bool,
    ) -> Result<Value> {
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 4000 {
            return Err("Введите поисковый запрос длиной до 4000 символов".into());
        }
        if !(1..=20).contains(&limit) {
            return Err("Количество результатов должно быть от 1 до 20".into());
        }
        let datasets = if native {
            self.library()?
        } else {
            self.datasets()?
        };
        let catalog: std::collections::BTreeMap<_, _> = array(&datasets["datasets"])
            .iter()
            .filter_map(|row| Some((row["id"].as_str()?, count(&row["declared_chunks"]))))
            .collect();
        let mut seen = HashSet::new();
        let mut selected = ids.unwrap_or_else(|| catalog.keys().map(|id| id.to_string()).collect());
        selected.retain(|id| seen.insert(id.clone()));
        if selected.iter().any(|id| !catalog.contains_key(id.as_str())) {
            return Err("Набор не найден среди пользовательских данных LES RAG".into());
        }
        let empty = if selected.is_empty() {
            Some(json!({"status":"empty"}))
        } else if !selected.iter().any(|id| catalog[id.as_str()] > 0) {
            Some(
                json!({"status":"not_ready","message":"В выбранных наборах пока нет поисковых фрагментов. Проверьте состав и обработку документов в LES RAG."}),
            )
        } else {
            None
        };
        if let Some(retrieval) = empty {
            return Ok(search_result(query, vec![], retrieval));
        }
        let result = self.request(Method::POST, "/api/search", Some(json!({
            "query":query,"dataset_ids":selected,"top_k":limit,"max_chars":1600,"include_context":true,"include_trace":true,
        })), 25)?;
        let hits = array(&result["chunks"])
            .iter()
            .map(|chunk| self.hit(chunk))
            .collect();
        let trace = &result["retrieval_trace"];
        let status = trace["status"].as_str().unwrap_or("unknown");
        let code = if status == "ok" {
            ""
        } else {
            text(&trace["error_code"])
        };
        let message = if status != "blocked" {
            ""
        } else {
            match code {
            "native_rrf_failed" => "Поиск по индексу сейчас недоступен. Проверьте состояние Qdrant и готовность документов в LES RAG.",
            "query_embedding_failed" => "Не удалось получить вектор запроса. Проверьте назначенную модель поиска в LES RAG.",
            _ => "Поиск сейчас недоступен. Проверьте состояние набора и модели поиска в LES RAG.",
        }
        };
        Ok(search_result(
            query,
            hits,
            json!({"fusion":trace["fusion"],"status":status,"error_code":code,"message":message}),
        ))
    }

    fn hit(&self, chunk: &Value) -> Value {
        let meta = &chunk["metadata"];
        let dataset = text(&meta["dataset_id"]);
        let document = text(&chunk["doc_name"]);
        let id = text(&chunk["doc_id"]);
        let link = self.document_link(id, dataset, document, text(&meta["source_sha256"]));
        let context = &chunk["context"];
        let fragments: Vec<Value> = array(&context["metadata"]["context_fragments"]).iter().map(|item|
            json!({"excerpt":text(&item["content"]),"page":item["metadata"]["page"],"point_id":item["metadata"]["qdrant_point_id"]})).collect();
        let section = first_text(&meta["section_heading"], &meta["parent_heading"]);
        json!({"rank":chunk["rank"],"document":document,"dataset_id":dataset,"document_id":id,
            "page":meta["page"],"section":(!section.is_empty()).then_some(section),"excerpt":text(&chunk["content"]),
            "context":text(&context["content"]),"context_fragments":fragments,"original_url":link})
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
fn search_result(query: &str, hits: Vec<Value>, retrieval: Value) -> Value {
    json!({"schema":"les.light.mcp.search.v1","query":query,"count":hits.len(),"hits":hits,"retrieval":retrieval})
}
