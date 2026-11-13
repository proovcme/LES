//! Explicit MCP operations on the owned LES API. No generic HTTP or filesystem tool.
use super::gateway::{Api, Result};
use serde_json::{json, Map, Value};

fn tool(
    name: &str,
    description: &str,
    read: bool,
    destructive: bool,
    required: &[&str],
    properties: Value,
) -> Value {
    json!({"name":name,"description":description,
        "annotations":{"readOnlyHint":read,"destructiveHint":destructive,"idempotentHint":read,"openWorldHint":matches!(name,"web_search"|"web_read")},
        "inputSchema":{"type":"object","required":required,"properties":properties,"additionalProperties":false}})
}

fn source_tool(name: &str, description: &str) -> Value {
    let registry: Value = serde_json::from_str(include_str!("../../../config/builtin-tools.json"))
        .expect("compiled source contracts");
    let contract = registry["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap();
    let mut schema = contract["input_schema"].clone();
    if name == "search_sources" {
        let properties = schema["properties"].as_object_mut().unwrap();
        let q = properties.remove("q").unwrap();
        properties.insert("query".into(), q);
        properties.get_mut("dataset_ids").unwrap()["type"] = json!(["array", "null"]);
        schema["required"] = json!(["query"]);
    }
    let mut value = tool(name, description, true, false, &[], json!({}));
    value["inputSchema"] = schema;
    value
}

pub fn list() -> Value {
    let dataset =
        json!({"type":"string","description":"ID пользовательского набора из list_datasets"});
    let project = json!({"type":"integer","minimum":1,"description":"ID проекта Леса"});
    let folder = json!({"type":"string","description":"Абсолютный путь в пределах папок, разрешённых в Лесе"});
    let area = json!({"type":"string","enum":["files","user","memory"]});
    let scope = json!({"type":"integer","minimum":0,"default":0});
    let path = json!({"type":"string","maxLength":240,"description":"Относительный путь файла в рабочем каталоге Леса"});
    json!({"tools":[
        tool("web_search","Найти публичные веб-источники. Возвращает названия, выдержки и прямые ссылки; выдержка поиска не заменяет чтение страницы.",true,false,&["q"],json!({"q":{"type":"string","minLength":3,"maxLength":500},"limit":{"type":"integer","minimum":1,"maximum":4}})),
        tool("web_read","Прочитать выбранную публичную веб-страницу. Возвращает текст, название и ссылку на первоисточник.",true,false,&["url"],json!({"url":{"type":"string","maxLength":8192},"max_chars":{"type":"integer","minimum":1,"maximum":4000}})),
        tool("list_datasets","Показать пользовательские наборы Леса и число готовых фрагментов.",true,false,&[],json!({})),
        tool("list_documents","Показать файлы набора, статусы обработки, размеры и ссылки на оригиналы.",true,false,&["dataset_id"],
            json!({"dataset_id":dataset,"q":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"offset":{"type":"integer","minimum":0}})),
        source_tool("search_sources","Найти источники: hybrid-поиск, дочитанные фрагменты, точные ссылки и редакция индекса. view=documents — только каталог, не доказательства. Продолжай через next_offset, сохраняя index_revision и параметры запроса. Модель ответов не вызывается."),
        source_tool("read_source","Дочитать найденный doc_id: индексированные разделы, таблицы и сохранённые части Word. Передавай source_sha256; для продолжения — next_offset и index_revision. coverage=bounded означает частичное чтение, не весь документ."),
        source_tool("read_pdf_source","Читать настоящую физическую страницу PDF по doc_id и page. Передавай source_sha256; длинную страницу дочитывай с next_offset до null. Ошибка чтения не подтверждает отсутствие информации."),
        source_tool("read_excel_source","Читать проверенные индексированные разделы книги, с листами и адресами ячеек. Это не вычисление формул и не полная выгрузка всех ячеек. Для последовательного чтения используй read_source."),
        source_tool("dataset_map","Получить каталог файлов набора. Имена и статусы не являются содержимым: нужный документ следует прочитать. Для продолжения каталога используй list_documents с тем же dataset_id и возвращённым next_offset."),
        tool("read_clause","Прочитать точный нумерованный пункт в выбранном документе с проверяемыми фрагментами.",true,false,&["dataset_id","document_id","number"],
            json!({"dataset_id":dataset,"document_id":{"type":"string","maxLength":240},"number":{"type":"string","pattern":"^[0-9]{1,5}(\\.[0-9]{1,5}){1,5}$"}})),
        tool("create_dataset","Создать пустой датасет. Папку можно подключить отдельным вызовом.",false,false,&["name"],
            json!({"name":{"type":"string","minLength":1,"maxLength":120}})),
        tool("attach_folder","Подключить разрешённую папку к датасету только для чтения. Возвращает ID фонового импорта.",false,false,&["dataset_id","path"],
            json!({"dataset_id":dataset,"path":folder,"index":{"type":"boolean","default":true}})),
        tool("intake_status","Показать ход импорта по его ID. Готовность отдельных файлов видна через list_documents.",true,false,&["id"],json!({"id":{"type":"string"}})),
        tool("list_projects","Показать проекты Леса.",true,false,&[],json!({})),
        tool("create_project","Создать проект Леса.",false,false,&["name"],json!({"name":{"type":"string"},"code":{"type":"string"},"address":{"type":"string"}})),
        tool("link_dataset","Подключить существующий пользовательский датасет к проекту.",false,false,&["project_id","dataset_id"],
            json!({"project_id":project,"dataset_id":dataset})),
        tool("workspace_read","Читать, перечислять рабочие файлы или читать историю/версию. USER.md и MEMORY.md доступны через area.",true,false,&["action","area"],
            json!({"action":{"enum":["list","read","history","version"]},"area":area,"project_id":scope,"path":path,
                   "offset":{"type":"integer","minimum":0},"history_id":{"type":"string"},"include_deleted":{"type":"boolean"}})),
        tool("workspace_write","Сохранить, отредактировать, удалить с историей или восстановить файл/память. Сначала workspace_read, затем передать его точную revision.",false,true,&["action","area","expected_revision"],
            json!({"action":{"enum":["save","edit","delete","restore"]},"area":area,"project_id":scope,"path":path,
                   "expected_revision":{"type":"string"},"text":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},
                   "history_id":{"type":"string"},"side":{"enum":["before","after"]},"reason":{"type":"string"}})),
        tool("read_chat_folder","Подготовить вложение папки для les_chat. Сканирует разрешённый источник в пределах лимитов чтения Леса.",false,false,&["path"],json!({"path":folder})),
        tool("les_chat","Начать или продолжить диалог в Лесе. Использует назначенную модель, историю, память, источники, навыки и инструменты чата. Может выполнять действия по вашему запросу.",false,true,&["question"],
            json!({"question":{"type":"string","minLength":1,"maxLength":20000},"session_id":{"type":"string"},"project_id":project,
                "dataset_ids":{"type":"array","items":{"type":"string"}},"target_files":{"type":"array","items":{"type":"string"}},
                "attachment_id":{"type":"string"},"reranker_enabled":{"type":"boolean"},
                "selected_sources_only":{"type":"boolean"},"mode":{"enum":["agent","search","chat"],"default":"agent"}}))
    ]})
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Заполните поле {key}."))
}

fn project_id(args: &Value) -> Result<Option<u64>> {
    match args.get("project_id") {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|id| (1..=i64::MAX as u64).contains(id))
            .map(Some)
            .ok_or_else(|| "Неверный ID проекта.".into()),
    }
}

fn dataset_ids(args: &Value) -> Result<Option<Vec<String>>> {
    if args["dataset_ids"].is_null() {
        return Ok(None);
    }
    args["dataset_ids"]
        .as_array()
        .filter(|items| items.len() <= 100)
        .ok_or("Укажите до 100 ID наборов.")?
        .iter()
        .map(|id| {
            id.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 240)
                .map(str::to_owned)
                .ok_or_else(|| "ID набора должен быть строкой.".into())
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn pick(args: &Value, keys: &[&str]) -> Map<String, Value> {
    keys.iter()
        .filter_map(|key| args.get(*key).map(|value| (key.to_string(), value.clone())))
        .collect()
}

fn query(args: &Value, keys: &[&str]) -> Result<Vec<(String, String)>> {
    pick(args, keys)
        .into_iter()
        .map(|(k, v)| {
            let value = match v {
                Value::String(s) => s,
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return Err(format!("Неверное поле {k}.")),
            };
            Ok((k, value))
        })
        .collect()
}

fn get(api: &Api, route: &str, args: &Value, keys: &[&str]) -> Result<Value> {
    let query = query(args, keys)?;
    api.get_with_query(
        route,
        &query
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
    )
}

fn absolute_folder(args: &Value) -> Result<&str> {
    let path = required(args, "path")?;
    if path.len() > 32_768 || !std::path::Path::new(path).is_absolute() {
        return Err("Укажите абсолютный путь к разрешённой папке.".into());
    }
    Ok(path)
}

fn chat(api: &Api, args: &Value) -> Result<Value> {
    let question = required(args, "question")?.trim();
    if question.chars().count() > 20_000 {
        return Err("Вопрос должен содержать не более 20 000 символов.".into());
    }
    let mode = args.get("mode").map_or(Some("agent"), Value::as_str);
    if !matches!(mode, Some("agent" | "search" | "chat")) {
        return Err("Выберите режим agent, search или chat.".into());
    }
    let project = project_id(args)?;
    let mut request = pick(
        args,
        &[
            "attachment_id",
            "target_files",
            "reranker_enabled",
            "selected_sources_only",
        ],
    );
    if let Some(ids) = dataset_ids(args)? {
        let catalog = api.library()?;
        if ids.iter().any(|id| {
            !catalog["datasets"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == *id))
        }) {
            return Err("Набор не найден среди пользовательских данных Леса.".into());
        }
        request.insert("dataset_ids".into(), json!(ids));
    }
    let session = match args.get("session_id") {
        None => api.create_chat_in_project(question, project)?,
        Some(value) => value
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 128)
            .ok_or("Неверный ID диалога.")?
            .to_owned(),
    };
    request.insert("question".into(), question.into());
    request.insert("session_id".into(), session.clone().into());
    request.insert("mode".into(), json!(mode));
    if let Some(id) = project {
        request.insert("project_id".into(), id.into());
    }
    let mut answer = api.post("/api/chat", Value::Object(request), 240)?;
    answer
        .as_object_mut()
        .ok_or("Лес вернул некорректный ответ чата.")?
        .insert("session_id".into(), session.into());
    Ok(answer)
}

pub fn call(name: &str, args: &Value) -> Result<Value> {
    let api = Api::connect()?;
    match name {
        "web_search" | "web_read" => {
            let (field, max) = if name == "web_search" {
                ("q", 500)
            } else {
                ("url", 8192)
            };
            let value = required(args, field)?;
            if value.chars().count() > max
                || (name == "web_search" && value.trim().chars().count() < 3)
            {
                return Err("Неверная длина запроса или адреса.".into());
            }
            let payload=api.post("/api/tools/call",json!({"tool":name,"args":Value::Object(pick(args,if name=="web_search"{&["q","limit"]}else{&["url","max_chars"]}))}),40)?;
            if payload["status"] == "error" || payload["status"] == "failed" {
                let message = payload["warnings"]
                    .as_array()
                    .and_then(|v| v.first())
                    .and_then(Value::as_str)
                    .or_else(|| payload["message"].as_str())
                    .unwrap_or("Не удалось получить данные из интернета. Повторите запрос.");
                return Err(message.to_string());
            }
            Ok(payload)
        }
        "list_datasets" => api.library(),
        "list_documents" => {
            required(args, "dataset_id")?;
            let mut docs = get(
                &api,
                "/api/light/library",
                args,
                &["dataset_id", "q", "limit", "offset"],
            )?;
            api.document_links(&mut docs);
            Ok(docs)
        }
        "search_sources" => {
            let mut input = args.as_object().ok_or("Укажите параметры поиска.")?.clone();
            input.remove("query");
            input.insert("q".into(), required(args, "query")?.into());
            // Retain the previously advertised null = all-user-datasets contract.
            if input.get("dataset_ids").is_some_and(Value::is_null) {
                input.remove("dataset_ids");
            }
            api.source_call(name, Value::Object(input))
        }
        "read_source" | "read_pdf_source" | "read_excel_source" | "dataset_map" => {
            api.source_call(name, args.clone())
        }
        "read_clause" => api.read_clause(
            required(args, "dataset_id")?,
            required(args, "document_id")?,
            required(args, "number")?,
        ),
        "create_dataset" => api.post(
            "/api/rag/datasets",
            json!({"name":required(args,"name")?}),
            25,
        ),
        "attach_folder" => {
            let id = required(args, "dataset_id")?;
            let path = absolute_folder(args)?;
            let index = args.get("index").map_or(Ok(true), |v| {
                v.as_bool().ok_or("index должен быть true или false.")
            })?;
            api.require_user_dataset(id)?;
            api.post(
                "/api/rag/index-external",
                json!({"dataset_id":id,"path":path,"parse":index,"background":true}),
                25,
            )
        }
        "intake_status" => {
            required(args, "id")?;
            get(&api, "/api/light/intake", args, &["id"])
        }
        "list_projects" => api.get("/api/projects"),
        "create_project" => {
            required(args, "name")?;
            api.post(
                "/api/projects",
                Value::Object(pick(args, &["name", "code", "address"])),
                25,
            )
        }
        "link_dataset" => {
            let id = required(args, "dataset_id")?;
            let project = project_id(args)?.ok_or("Укажите ID проекта.")?;
            api.require_user_dataset(id)?;
            api.post(
                &format!("/api/projects/{project}/links"),
                json!({"kind":"dataset","ref":id}),
                25,
            )
        }
        "workspace_read" => {
            required(args, "action")?;
            required(args, "area")?;
            get(
                &api,
                "/api/light/agent-workspace",
                args,
                &[
                    "action",
                    "area",
                    "project_id",
                    "path",
                    "offset",
                    "history_id",
                    "include_deleted",
                ],
            )
        }
        "workspace_write" => api.post(
            "/api/light/agent-workspace",
            Value::Object(pick(
                args,
                &[
                    "action",
                    "area",
                    "project_id",
                    "path",
                    "expected_revision",
                    "text",
                    "old_text",
                    "new_text",
                    "history_id",
                    "side",
                    "reason",
                ],
            )),
            25,
        ),
        "read_chat_folder" => api.post(
            "/api/rag/attach-folder",
            json!({"path":absolute_folder(args)?}),
            120,
        ),
        "les_chat" => chat(&api, args),
        _ => Err("Неизвестный инструмент Леса.".into()),
    }
}
