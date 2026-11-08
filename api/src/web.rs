//! Explicit web tools shared by the retained chat/MCP facade and the native agent.
use crate::{json_response, Context};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{Method, StatusCode},
    response::Response,
};
use futures_util::StreamExt;
use reqwest::Url;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    net::{IpAddr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

pub const PATH: &str = "/api/light/internal-web";
const DDG: &str = "https://html.duckduckgo.com/html/";
fn clean(value: &str, limit: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}
fn url(raw: &str) -> Result<Url, &'static str> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) || raw.contains('\\') {
        return Err("WEB_URL_NOT_PUBLIC");
    }
    let mut url = Url::parse(raw).map_err(|_| "WEB_URL_NOT_PUBLIC")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("WEB_URL_NOT_PUBLIC");
    }
    url.set_fragment(None);
    Ok(url)
}
fn public(ip: IpAddr) -> bool {
    crate::model_runtime::permitted(ip, "remote").is_ok()
        && !les_core::trust::ip_in_networks(
            &ip.to_string(),
            &["100.64.0.0/10".into(), "224.0.0.0/4".into()],
        )
        && !matches!(ip,IpAddr::V6(v) if v.to_ipv4_mapped().is_some() || v.segments()[0] & 0xe000 != 0x2000)
}
async fn addresses(url: &Url, public_only: bool) -> Result<Vec<IpAddr>, &'static str> {
    let host = url
        .host_str()
        .ok_or("WEB_URL_NOT_PUBLIC")?
        .trim_matches(['[', ']'])
        .to_string();
    let port = url.port_or_known_default().ok_or("WEB_URL_NOT_PUBLIC")?;
    let addresses = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|v| v.map(|a| a.ip()).collect::<Vec<_>>())
                .map_err(|_| "WEB_URL_RESOLUTION_FAILED")
        }),
    )
    .await
    .map_err(|_| "WEB_TIMEOUT")?
    .map_err(|_| "WEB_URL_RESOLUTION_FAILED")??;
    if addresses.is_empty() || addresses.len() > 64 {
        return Err("WEB_URL_RESOLUTION_FAILED");
    }
    if addresses.iter().any(|ip| {
        if public_only {
            !public(*ip)
        } else {
            // Only an explicitly configured search service may be private.
            ["loopback", "private_network", "remote"]
                .iter()
                .all(|scope| crate::model_runtime::permitted(*ip, scope).is_err())
        }
    }) {
        return Err("WEB_URL_NOT_PUBLIC");
    }
    Ok(addresses)
}
fn direct(raw: &str) -> Option<Url> {
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_string()
    };
    let mut parsed = url(&raw).ok()?;
    let host = parsed.host_str()?;
    if host == "duckduckgo.com" || host.ends_with(".duckduckgo.com") {
        let target = parsed
            .query_pairs()
            .find(|(k, _)| k == "uddg")?
            .1
            .into_owned();
        parsed = url(&target).ok()?;
    }
    // Search links need not be contacted, but numeric private destinations are never evidence.
    if parsed
        .host_str()?
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|ip| !public(ip))
    {
        return None;
    }
    if parsed.host_str()?.eq_ignore_ascii_case("localhost") {
        return None;
    }
    Some(parsed)
}
fn rows(raw: &[Value], limit: usize) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for row in raw {
        let Some(url) = row["url"].as_str().and_then(direct) else {
            continue;
        };
        if !seen.insert(url.to_string()) {
            continue;
        }
        let domain = url
            .host_str()
            .unwrap_or("")
            .trim_start_matches("www.")
            .to_string();
        let title = clean(row["title"].as_str().unwrap_or(&domain), 240);
        rows.push(json!({"title":if title.is_empty(){domain.clone()}else{title},"snippet":clean(row["snippet"].as_str().or(row["content"].as_str()).unwrap_or(""),700),"domain":domain,"url":url.as_str()}));
        if rows.len() >= limit.clamp(1, 12) {
            break;
        }
    }
    rows
}
async fn fetch(mut target: Url, public_only: bool) -> Result<(Url, String, String), &'static str> {
    for hop in 0..=3 {
        let peers = addresses(&target, public_only).await?;
        let mut reply =
            crate::model_http::get(target.clone(), peers, Duration::from_secs(15)).await?;
        if reply.status.is_redirection() {
            if hop == 3 {
                return Err("WEB_REDIRECT_LIMIT");
            }
            let next = target
                .join(reply.location.as_deref().ok_or("WEB_REDIRECT_INVALID")?)
                .map_err(|_| "WEB_REDIRECT_INVALID")?;
            let next = url(next.as_str())?;
            // A configured local SearXNG never redirects to another server.
            if !public_only && next.origin() != target.origin() {
                return Err("WEB_REDIRECT_INVALID");
            }
            target = next;
            continue;
        }
        if reply.status == StatusCode::TOO_MANY_REQUESTS || reply.status == StatusCode::FORBIDDEN {
            return Err("WEB_SEARCH_RATE_LIMITED");
        }
        if reply.status == StatusCode::ACCEPTED {
            return Err("WEB_SEARCH_CHALLENGE");
        }
        if !reply.status.is_success() {
            return Err("WEB_HTTP_ERROR");
        }
        let content_type = reply.content_type.unwrap_or_default();
        if ![
            "text/html",
            "application/xhtml+xml",
            "text/plain",
            "application/json",
            "application/rss+xml",
            "application/xml",
            "text/xml",
        ]
        .iter()
        .any(|v| content_type.to_ascii_lowercase().starts_with(v))
        {
            return Err("WEB_CONTENT_TYPE_UNSUPPORTED");
        }
        let mut bytes = Vec::new();
        while let Some(part) = reply.source.next().await {
            let part = part?;
            if bytes.len() + part.len() > 2 * 1024 * 1024 {
                return Err("WEB_RESPONSE_TOO_LARGE");
            }
            bytes.extend_from_slice(&part);
        }
        let text = les_core::web::decode(&bytes, &content_type)?;
        return Ok((target, text, content_type));
    }
    Err("WEB_REDIRECT_LIMIT")
}
async fn search(value: &Value) -> Result<Value, &'static str> {
    let query = clean(value["q"].as_str().ok_or("WEB_QUERY_INVALID")?, 500);
    let extended = value["mode"] == "extended";
    let limit = value["limit"].as_u64().unwrap_or(4).clamp(1, 12) as usize;
    let mut provider = if extended { "searxng" } else { "duckduckgo" };
    let mut degraded = false;
    let mut fallback_code = "";
    let requested = if extended { "extended" } else { "simple" };
    if query.chars().count() < 3 {
        return Ok(
            json!({"status":"missing","query":query,"results":[],"missing":["query"],"requested_mode":requested,"effective_mode":requested,"provider":provider,"degraded":false,"fallback_reason":""}),
        );
    }
    let mut result = None;
    if extended {
        let attempt = async {
            let base = value["searxng_url"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or("SEARXNG_URL_MISSING")?;
            let mut target = url(&format!("{}/search", base.trim_end_matches('/')))?;
            target
                .query_pairs_mut()
                .append_pair("q", &query)
                .append_pair("format", "json")
                .append_pair("language", "ru");
            let (_, body, _) = fetch(target, false).await?;
            let body: Value =
                serde_json::from_str(&body).map_err(|_| "SEARXNG_RESPONSE_INVALID")?;
            Ok::<_, &'static str>(rows(
                body["results"]
                    .as_array()
                    .ok_or("SEARXNG_RESPONSE_INVALID")?,
                limit,
            ))
        }
        .await;
        match attempt {
            Ok(rows) => result = Some(rows),
            Err(_) => {
                degraded = true;
                provider = "duckduckgo";
            }
        }
    }
    let results = if let Some(result) = result {
        result
    } else {
        let attempt = async {
            let mut target = url(DDG)?;
            target
                .query_pairs_mut()
                .append_pair("q", &query)
                .append_pair("kl", "ru-ru");
            let (_, body, _) = fetch(target, true).await?;
            let parsed = les_core::web::search(&body, 12)?;
            Ok::<_, &'static str>(rows(
                parsed.as_array().ok_or("WEB_RESPONSE_INVALID")?,
                limit,
            ))
        }
        .await;
        match attempt {
            Ok(rows) => rows,
            Err(code) => {
                let mut target = url("https://www.bing.com/search")?;
                target
                    .query_pairs_mut()
                    .append_pair("q", &query)
                    .append_pair("format", "rss")
                    .append_pair("setlang", "ru");
                let (_, body, _) = fetch(target, true).await?;
                let parsed = les_core::web::rss(&body, 12)?;
                let results = rows(
                    parsed.as_array().ok_or("WEB_SEARCH_RESPONSE_INVALID")?,
                    limit,
                );
                provider = "bing";
                degraded = true;
                fallback_code = code;
                results
            }
        }
    };
    Ok(
        json!({"status":if results.is_empty(){"missing"}else{"ok"},"query":query,"results":results,"missing":if results.is_empty(){vec!["Поиск не вернул результатов"]}else{vec![]},"requested_mode":requested,"effective_mode":if degraded{"simple"}else{requested},"provider":provider,"degraded":degraded,"fallback_code":fallback_code,"fallback_reason":if provider=="bing"{"Основной поиск недоступен; результаты получены через Bing."}else if degraded{"Расширенный поиск недоступен, использован резервный простой."}else{""},"retrieved_at":les_core::web::now()}),
    )
}
async fn read(value: &Value) -> Result<Value, &'static str> {
    let requested = value["url"].as_str().ok_or("WEB_URL_NOT_PUBLIC")?;
    let (final_url, body, content_type) = fetch(url(requested)?, true).await?;
    let limit = value["max_chars"].as_u64().unwrap_or(4000).clamp(1, 4000) as usize;
    let mut parsed = if content_type.to_ascii_lowercase().starts_with("text/plain") {
        json!({"title":final_url.host_str(),"text":body.chars().take(limit).collect::<String>(),"text_truncated":body.chars().count()>limit})
    } else if content_type
        .to_ascii_lowercase()
        .starts_with("application/json")
    {
        return Err("WEB_CONTENT_TYPE_UNSUPPORTED");
    } else {
        if !content_type.to_ascii_lowercase().starts_with("text/html")
            && !content_type
                .to_ascii_lowercase()
                .starts_with("application/xhtml+xml")
        {
            return Err("WEB_CONTENT_TYPE_UNSUPPORTED");
        }
        les_core::web::page(&body, limit)?
    };
    let has_text = parsed["text"].as_str().is_some_and(|v| !v.is_empty());
    parsed["status"] = json!(if has_text { "ok" } else { "missing" });
    parsed["requested_url"] = json!(requested);
    parsed["final_url"] = json!(final_url.as_str());
    parsed["retrieved_at"] = json!(les_core::web::now());
    parsed["sources"] = if has_text {
        json!([{"kind":"web_page","url":final_url.as_str(),"title":parsed["title"]}])
    } else {
        json!([])
    };
    parsed["missing"] = if has_text {
        json!([])
    } else {
        json!(["Страница не вернула читаемый текст"])
    };
    parsed["warnings"] = json!([]);
    Ok(parsed)
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != Method::POST
        || context
            .local_model
            .as_ref()
            .is_none_or(|b| !b.authorized(request.headers()))
    {
        return crate::error(StatusCode::NOT_FOUND, "Not Found");
    }
    let Ok(_permit) = context.workspace_slots.clone().try_acquire_owned() else {
        return json_response(
            StatusCode::OK,
            json!({"status":"error","code":"WEB_BUSY","results":[],"sources":[],"warnings":["Поиск занят. Повторите запрос через несколько секунд."],"missing":[]}),
            "rust.web",
        );
    };
    let bytes = match to_bytes(request.into_body(), 32 * 1024).await {
        Ok(v) => v,
        Err(_) => return crate::error(StatusCode::PAYLOAD_TOO_LARGE, "WEB_REQUEST_TOO_LARGE"),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return crate::error(StatusCode::BAD_REQUEST, "WEB_REQUEST_INVALID"),
    };
    if value["schema"] != "les.web.v1"
        || !matches!(value["operation"].as_str(), Some("search" | "read"))
    {
        return crate::error(StatusCode::BAD_REQUEST, "WEB_REQUEST_INVALID");
    }
    let payload = execute(&value).await;
    json_response(StatusCode::OK, payload, "rust.web")
}
pub(crate) async fn execute(value: &Value) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        if value["operation"] == "search" {
            search(&value).await
        } else {
            read(&value).await
        }
    })
    .await
    .unwrap_or(Err("WEB_TIMEOUT"));
    let payload = match result {
        Ok(v) => v,
        Err(code) => {
            eprintln!("{}", json!({"event":"web_tool_failed","code":code}));
            let message=match code {"WEB_SEARCH_CHALLENGE"=>"Поисковик запросил проверку человека. Повторите позже или подключите SearXNG.","WEB_SEARCH_RATE_LIMITED"=>"Поисковик временно ограничил запросы. Повторите позже.","WEB_URL_NOT_PUBLIC"=>"Можно читать только публичные веб-страницы.","WEB_TIMEOUT"|"UPSTREAM_TIMEOUT"=>"Сайт не ответил вовремя. Повторите запрос.","WEB_ENCODING_UNSUPPORTED"=>"Кодировка страницы пока не поддерживается.","WEB_CONTENT_TYPE_UNSUPPORTED"=>"Этот адрес не вернул читаемую веб-страницу.",_=>"Не удалось получить данные из интернета. Проверьте подключение и повторите запрос."};
            json!({"status":"error","code":code,"query":value["q"],"requested_url":value["url"],"results":[],"sources":[],"text":"","title":"","final_url":"","missing":[],"warnings":[message]})
        }
    };
    payload
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_are_decoded_once_and_bounded() {
        assert_eq!(
            direct("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fa%252Fb")
                .unwrap()
                .as_str(),
            "https://example.org/a%2Fb"
        );
        assert!(direct("javascript:alert(1)").is_none());
        assert!(direct("https://user:password@example.org").is_none());
        assert!(direct("http://127.0.0.1/x").is_none());
        assert_eq!(
            rows(
                &[
                    json!({"url":"https://example.org","title":"Документ"}),
                    json!({"url":"https://example.org/"})
                ],
                4
            )
            .len(),
            1
        );
    }
    #[test]
    fn public_pages_exclude_internal_services() {
        for raw in [
            "127.0.0.1",
            "10.1.1.1",
            "100.100.100.200",
            "100.64.1.1",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "224.1.1.1",
            "::2",
            "fec0::1",
        ] {
            assert!(!public(raw.parse().unwrap()), "{raw}");
        }
        assert!(public("1.1.1.1".parse().unwrap()));
    }
}
