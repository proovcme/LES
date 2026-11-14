//! Explicit loopback companion bridge; snapshots use the ordinary indexing queue.
mod registered;
use crate::{auth, error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc, time::Duration};

pub const PATH: &str = "/api/integrations/uvyazka";
const MAX_BYTES: usize = 24 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;
struct Companion {
    url: reqwest::Url,
    client: reqwest::Client,
}

fn endpoint(raw: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(raw).map_err(|_| "ENGINEERING_ENDPOINT_INVALID")?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_none()
    {
        return Err("ENGINEERING_ENDPOINT_INVALID".into());
    }
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| "ENGINEERING_ENDPOINT_INVALID")?;
    }
    if !url
        .host_str()
        .and_then(|s| s.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback())
    {
        return Err("ENGINEERING_ENDPOINT_INVALID".into());
    }
    Ok(url)
}
impl Companion {
    fn new(raw: &str) -> Result<Self> {
        Ok(Self {
            url: endpoint(raw)?,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(45))
                .build()
                .map_err(|_| "ENGINEERING_UNAVAILABLE")?,
        })
    }
    async fn get(&self, path: &str, fields: &[(&str, String)]) -> Result<Value> {
        let mut url = self
            .url
            .join(path)
            .map_err(|_| "ENGINEERING_ENDPOINT_INVALID")?;
        url.query_pairs_mut()
            .extend_pairs(fields.iter().map(|(k, v)| (*k, v.as_str())));
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| "ENGINEERING_UNAVAILABLE")?;
        if !response.status().is_success() {
            return Err("ENGINEERING_SOURCE_UNAVAILABLE".into());
        }
        if response.content_length().is_some_and(|n| n > 1024 * 1024) {
            return Err("ENGINEERING_RESPONSE_LIMIT".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "ENGINEERING_UNAVAILABLE")?
        {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err("ENGINEERING_RESPONSE_LIMIT".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "ENGINEERING_RESPONSE_INVALID".into())
    }
    async fn object(
        &self,
        project: &str,
        dataset: &str,
        object: &str,
        verify: bool,
    ) -> Result<(Value, Value)> {
        let (mut offset, mut text) = (0u64, String::new());
        let mut first = Value::Null;
        loop {
            let part = self
                .get(
                    "/api/models/object",
                    &[
                        ("project_id", project.into()),
                        ("dataset_id", dataset.into()),
                        ("object_id", object.into()),
                        ("max_chars", "12000".into()),
                        ("offset", offset.to_string()),
                        (
                            "verify_source",
                            if verify && offset == 0 { "1" } else { "0" }.into(),
                        ),
                    ],
                )
                .await?;
            if part["project_id"] != project
                || part["dataset_id"] != dataset
                || part["object_id"] != object
            {
                return Err("ENGINEERING_IDENTITY_MISMATCH".into());
            }
            let fragment = part["text"]
                .as_str()
                .ok_or("ENGINEERING_RESPONSE_INVALID")?;
            if text.len() + fragment.len() > 256 * 1024 {
                return Err("ENGINEERING_OBJECT_LIMIT".into());
            }
            let next = offset + fragment.chars().count() as u64;
            if offset == 0 {
                first = part.clone();
            } else if part["source"] != first["source"] {
                return Err("ENGINEERING_SOURCE_CHANGED".into());
            }
            text.push_str(fragment);
            if part["next_offset"].is_null() {
                if part["total_chars"].as_u64() != Some(next) {
                    return Err("ENGINEERING_SNAPSHOT_INCOMPLETE".into());
                }
                break;
            }
            if next <= offset || part["next_offset"].as_u64() != Some(next) {
                return Err("ENGINEERING_CURSOR_INVALID".into());
            }
            offset = next;
        }
        Ok((
            serde_json::from_str(&text).map_err(|_| "ENGINEERING_RESPONSE_INVALID")?,
            first,
        ))
    }
    async fn snapshot(&self, project: &str, dataset: &str) -> Result<Value> {
        let (report, checked) = self.object(project, dataset, "@dataset", true).await?;
        if checked["freshness"]["status"] != "content_hash_matches" {
            return Err("ENGINEERING_SOURCE_CHANGED".into());
        }
        let count = report["object_count"]
            .as_u64()
            .ok_or("ENGINEERING_RESPONSE_INVALID")? as usize;
        if count == 0 || count > les_core::engineering::MAX_OBJECTS {
            return Err("ENGINEERING_SNAPSHOT_LIMIT".into());
        }
        let counts = report["counts"]
            .as_object()
            .ok_or("ENGINEERING_RESPONSE_INVALID")?;
        let mut objects = Vec::with_capacity(count);
        let (mut seen, mut size) = (HashSet::new(), 0);
        for (section, expected) in counts {
            let expected = expected.as_u64().ok_or("ENGINEERING_RESPONSE_INVALID")? as usize;
            let (mut cursor, mut actual) = (None::<String>, 0usize);
            let mut cursors = HashSet::new();
            loop {
                let mut fields = vec![
                    ("project_id", project.into()),
                    ("dataset_id", dataset.into()),
                    ("section", section.clone()),
                    ("limit", "100".into()),
                ];
                if let Some(cursor) = &cursor {
                    fields.push(("cursor", cursor.clone()));
                }
                let page = self.get("/api/models/query", &fields).await?;
                if page["source"] != checked["source"]
                    || page["project_id"] != project
                    || page["dataset_id"] != dataset
                {
                    return Err("ENGINEERING_SOURCE_CHANGED".into());
                }
                for row in page["items"]
                    .as_array()
                    .ok_or("ENGINEERING_RESPONSE_INVALID")?
                {
                    let id = row["object_id"]
                        .as_str()
                        .ok_or("ENGINEERING_RESPONSE_INVALID")?;
                    if !seen.insert(id.to_owned()) || objects.len() >= count {
                        return Err("ENGINEERING_SNAPSHOT_INCOMPLETE".into());
                    }
                    let (object, part) = self.object(project, dataset, id, false).await?;
                    if object["object_id"] != id
                        || object["section"] != *section
                        || part["source"] != checked["source"]
                    {
                        return Err("ENGINEERING_IDENTITY_MISMATCH".into());
                    }
                    size += serde_json::to_vec(&object)
                        .map_err(|_| "ENGINEERING_RESPONSE_INVALID")?
                        .len();
                    if size > MAX_BYTES {
                        return Err("ENGINEERING_SNAPSHOT_LIMIT".into());
                    }
                    objects.push(object);
                    actual += 1;
                }
                if page["next_cursor"].is_null() {
                    break;
                }
                let next = page["next_cursor"]
                    .as_str()
                    .ok_or("ENGINEERING_CURSOR_INVALID")?
                    .to_owned();
                if !cursors.insert(next.clone()) || cursors.len() > 1000 {
                    return Err("ENGINEERING_CURSOR_INVALID".into());
                }
                cursor = Some(next);
            }
            if actual != expected {
                return Err("ENGINEERING_SNAPSHOT_INCOMPLETE".into());
            }
        }
        let (_, after) = self.object(project, dataset, "@dataset", true).await?;
        if after["source"] != checked["source"]
            || after["freshness"]["status"] != "content_hash_matches"
        {
            return Err("ENGINEERING_SOURCE_CHANGED".into());
        }
        let mut source = report["source"].clone();
        source["filename"] = json!(source["relative_path"]
            .as_str()
            .and_then(|p| p.rsplit(['/', '\\']).next())
            .ok_or("ENGINEERING_RESPONSE_INVALID")?);
        let value = json!({"schema":les_core::engineering::SCHEMA,"object_count":count,"objects":objects,
            "origin":{"project_id":project,"dataset_id":dataset,"source":source,"reader":report["reader"],"coverage":report["coverage"]}});
        les_core::engineering::validate(&value).map_err(str::to_owned)?;
        Ok(value)
    }
}
fn id<'a>(body: &'a Value, field: &str) -> Result<&'a str> {
    les_core::engineering::identity(&body[field]).map_err(str::to_owned)
}
async fn perform(context: Arc<Context>, body: Value) -> Result<Value> {
    let companion = Companion::new(body["url"].as_str().ok_or("ENGINEERING_ENDPOINT_INVALID")?)?;
    match body["action"].as_str() {
        Some("projects") => {
            let value = companion.get("/api/projects", &[]).await?;
            let projects = value["projects"]
                .as_array()
                .ok_or("ENGINEERING_RESPONSE_INVALID")?
                .iter()
                .map(|p| json!({"id":p["id"],"name":p["name"]}))
                .collect::<Vec<_>>();
            Ok(json!({"projects":projects,"url":companion.url.as_str()}))
        }
        Some("datasets") => {
            companion
                .get(
                    "/api/models/datasets",
                    &[
                        ("project_id", id(&body, "project_id")?.into()),
                        ("offset", body["offset"].as_u64().unwrap_or(0).to_string()),
                        ("limit", "50".into()),
                    ],
                )
                .await
        }
        Some("sources") => companion.registered_sources(&body).await,
        Some("import" | "read_import") => {
            if context.intake.is_none() {
                return Err("ATTACHMENT_INDEX_UNAVAILABLE".into());
            }
            let _slot = context
                .upload_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| "ATTACHMENT_QUEUE_BUSY")?;
            let project = id(&body, "project_id")?;
            let dataset = if body["action"] == "read_import" {
                companion.read_registered(&body).await?
            } else {
                id(&body, "dataset_id")?.to_owned()
            };
            let value = companion.snapshot(project, &dataset).await?;
            let bytes = serde_json::to_vec(&value).map_err(|_| "ENGINEERING_RESPONSE_INVALID")?;
            if bytes.len() > MAX_BYTES {
                return Err("ENGINEERING_SNAPSHOT_LIMIT".into());
            }
            let hash = les_core::sha256_hex(&bytes);
            let title = value["origin"]["source"]["filename"]
                .as_str()
                .ok_or("ENGINEERING_RESPONSE_INVALID")?
                .chars()
                .filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c))
                .take(50)
                .collect::<String>();
            let name = format!(
                "Uvyazka-{}-{}.json",
                title.trim_matches([' ', '.']),
                &hash[..12]
            );
            let directory = crate::attachments::scratch(&context).await?;
            let ident = les_core::chat_attachments::new_id().map_err(str::to_owned)?;
            let nonce = ident
                .strip_prefix("read_")
                .ok_or("ATTACHMENT_STORE_UNAVAILABLE")?;
            let path = directory.join(format!("native-read_{nonce}.json"));
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .await
                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
            let temp = crate::attachments::Temporary(path.clone());
            file.write_all(&bytes)
                .await
                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
            file.sync_all()
                .await
                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
            drop(file);
            let mut queued = crate::intake::queue_attachment(
                context,
                temp,
                name,
                "uvyazka-explicit-import".into(),
                Some(hash),
            )
            .await?;
            queued["engineering_origin"] = value["origin"].clone();
            queued["notice"] = json!("Снимок поставлен в общую очередь индексации. Готовность поиска появится после обработки; исходный файл не изменён.");
            Ok(queued)
        }
        Some("viewer") => {
            let capabilities = companion
                .get("/simulation/les-capabilities.json", &[])
                .await
                .map_err(|code| {
                    if code == "ENGINEERING_UNAVAILABLE" {
                        code
                    } else {
                        "ENGINEERING_VIEWER_UPDATE_REQUIRED".into()
                    }
                })?;
            if capabilities["contract"] != "uvyazka.les-viewer.v1" {
                return Err("ENGINEERING_VIEWER_UPDATE_REQUIRED".into());
            }
            let target = &body["target"];
            let project = id(target, "project_id")?;
            let dataset = id(target, "dataset_id")?;
            let object = id(target, "object_id")?;
            let hash =
                les_core::engineering::digest(&target["source_sha256"]).map_err(str::to_owned)?;
            let (_, part) = companion.object(project, dataset, object, true).await?;
            if part["source"]["sha256"] != hash
                || part["freshness"]["status"] != "content_hash_matches"
            {
                return Err("ENGINEERING_SOURCE_CHANGED".into());
            }
            let mut url = companion
                .url
                .join("simulation/")
                .map_err(|_| "ENGINEERING_ENDPOINT_INVALID")?;
            url.query_pairs_mut().extend_pairs([
                ("project", project),
                ("les_dataset", dataset),
                ("les_object", object),
                ("les_sha256", hash),
            ]);
            Ok(
                json!({"url":url.as_str(),"target":target,"notice":"Откроется отдельное окно Увязки; наличие геометрии определяется её читателем."}),
            )
        }
        _ => Err("ENGINEERING_ACTION_INVALID".into()),
    }
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != axum::http::Method::POST {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "Используйте явное действие подключения.",
        );
    }
    let cors = match auth::authorize(&context, request.headers(), true).await {
        Ok(v) => v,
        Err(v) => return v,
    };
    let result = match to_bytes(request.into_body(), 16 * 1024).await {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(body) => {
                match tokio::time::timeout(Duration::from_secs(180), perform(context, body)).await {
                    Ok(result) => result,
                    Err(_) => Err("ENGINEERING_TIMEOUT".into()),
                }
            }
            Err(_) => Err("ENGINEERING_REQUEST_INVALID".into()),
        },
        Err(_) => Err("ENGINEERING_REQUEST_INVALID".into()),
    };
    let mut response = match result {
        Ok(value) => json_response(StatusCode::OK, value, "rust.uvyazka"),
        Err(code) => json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"code":code,"detail":message(&code)}),
            "rust.uvyazka",
        ),
    };
    response.headers_mut().extend(cors);
    response
}
fn message(code: &str) -> &'static str {
    match code {
        "ENGINEERING_ENDPOINT_INVALID"=>"Укажите локальный адрес Увязки с портом, например http://127.0.0.1:8765/.",
        "ENGINEERING_UNAVAILABLE"=>"Увязка не отвечает. Запустите её и проверьте адрес.",
        "ENGINEERING_PERMISSION_DENIED"=>"Увязка отклонила чтение. Проверьте её локальную панель и совместимость подключения; исходники не изменены.",
        "ENGINEERING_READ_FAILED"=>"Увязка не смогла прочитать выбранный файл. Проверьте оригинал и установленный читатель; данные не поставлены в индекс.",
        "ENGINEERING_READ_TIMEOUT"=>"Увязка не успела ответить. Чтение может продолжаться в ней; повторите действие. Неполные данные в индекс не отправлены.",
        "ENGINEERING_VIEWER_UPDATE_REQUIRED"=>"Для открытия точного объекта нужен вьювер Увязки с поддержкой ссылок LES. Обновите Увязку; ссылка на другой объект не подставляется.",
        "ENGINEERING_SOURCE_CHANGED"=>"Редакция исходного файла изменилась или недоступна. Обновите снимок в Увязке; старые данные не опубликованы заново.",
        "ENGINEERING_SNAPSHOT_LIMIT"|"ENGINEERING_OBJECT_LIMIT"|"ENGINEERING_RESPONSE_LIMIT"=>"Снимок превышает предел этого подключения. Ничего не обрезано и не поставлено в индекс.",
        "ENGINEERING_TIMEOUT"=>"Не удалось завершить чтение за три минуты. Проверьте Увязку и повторите подключение.",
        "ATTACHMENT_INDEX_UNAVAILABLE"=>"В этом экземпляре Леса не запущена индексация.",
        _=>"Не удалось проверить снимок Увязки. Проверьте чтение исходника и выбранный проект; неполные данные не ставятся в индекс."
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn companion_is_explicit_loopback_without_redirectable_user_input() {
        assert_eq!(
            endpoint("http://localhost:8765/").unwrap().host_str(),
            Some("127.0.0.1")
        );
        for bad in [
            "https://example.org/",
            "http://127.0.0.1/",
            "http://user@127.0.0.1:8765/",
            "http://127.0.0.1:8765/api/",
            "http://127.0.0.1:8765/?token=x",
            "http://example.org:8765/",
        ] {
            assert!(endpoint(bad).is_err(), "{bad}");
        }
    }
    async fn fixture(changed: bool, incomplete: bool) -> (Companion, tokio::task::JoinHandle<()>) {
        use axum::{routing::any, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let checks = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback(any(move |request: Request| {
            let checks = checks.clone();
            async move {
                let url = reqwest::Url::parse(&format!("http://127.0.0.1{}",request.uri())).unwrap();
                let q:std::collections::HashMap<_,_> = url.query_pairs().map(|(k,v)|(k.into_owned(),v.into_owned())).collect();
                let source = json!({"sha256":"a".repeat(64),"relative_path":"Models/Example.rvt","format":"rvt","doc_id":"doc-1"});
                let value = if url.path() == "/api/models/query" {
                    let (items,next) = if q["section"] == "levels" { (json!([]),Value::Null) }
                        else if !q.contains_key("cursor") { (json!([]),json!("continue-empty-page")) }
                        else { (json!([{"object_id":"elements:42"}]),Value::Null) };
                    json!({"project_id":"example","dataset_id":"snapshot","source":source,"items":items,"next_cursor":next})
                } else {
                    let object = &q["object_id"];
                    let full = if object == "@dataset" {
                        json!({"source":source,"reader":{"engine":"native","revision":"v1"},"coverage":{"status":"partial","limitations":["Saved metadata only"]},
                            "object_count":if incomplete {2}else{1},"counts":{"elements":if incomplete {2}else{1},"levels":0}})
                    } else { json!({"object_id":"elements:42","native_id":"42","section":"elements","name":"Щит ЩР","category":"Электрооборудование","identifiers":{"element_id":"42"},
                        "properties":{"power":{"value":0,"unit":"kW"},"missing":null}}) };
                    let text=full.to_string(); let offset=q["offset"].parse::<usize>().unwrap();
                    let total=text.chars().count();let part=text.chars().skip(offset).take(47).collect::<String>();
                    let end=offset+part.chars().count();
                    let stale=object=="@dataset" && q["verify_source"]=="1" && checks.fetch_add(1,Ordering::SeqCst)>0 && changed;
                    json!({"project_id":"example","dataset_id":"snapshot","object_id":object,"source":source,
                        "freshness":{"status":if stale {"needs_refresh"} else {"content_hash_matches"}},"text":part,"total_chars":total,"next_offset":if end<total {json!(end)}else{Value::Null}})
                };
                json_response(StatusCode::OK,value,"synthetic.uvyazka")
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            Companion::new(&format!("http://{address}/")).unwrap(),
            server,
        )
    }
    #[test]
    fn all_cursor_pages_and_typed_properties_are_read_before_publication() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (client, server) = fixture(false, false).await;
                let value = client.snapshot("example", "snapshot").await.unwrap();
                assert_eq!(value["objects"].as_array().unwrap().len(), 1);
                assert_eq!(value["objects"][0]["properties"]["power"]["value"], 0);
                assert!(value["objects"][0]["properties"]["missing"].is_null());
                assert_eq!(value["origin"]["source"]["filename"], "Example.rvt");
                let cards = les_core::engineering::cards(&value).unwrap();
                assert_eq!(cards[0]["model_locator"]["native_id"], "42");
                server.abort();
            });
    }
    #[test]
    fn source_change_or_missing_objects_rejects_the_entire_import() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for (changed, incomplete, expected) in [
                    (true, false, "ENGINEERING_SOURCE_CHANGED"),
                    (false, true, "ENGINEERING_SNAPSHOT_INCOMPLETE"),
                ] {
                    let (client, server) = fixture(changed, incomplete).await;
                    assert_eq!(
                        client.snapshot("example", "snapshot").await.unwrap_err(),
                        expected
                    );
                    server.abort();
                }
            });
    }
}
