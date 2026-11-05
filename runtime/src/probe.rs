use crate::config::{Failure, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Read,
    time::{Duration, Instant},
};

pub fn owned_addresses(status: &Value) -> Option<(reqwest::Url, reqwest::Url, String)> {
    let api = reqwest::Url::parse(status["api_url"].as_str()?).ok()?;
    let ui = reqwest::Url::parse(status["ui_url"].as_str()?).ok()?;
    let instance = status["instance_id"].as_str()?.to_owned();
    if instance.is_empty() || instance.len() > 256 {
        return None;
    }
    for url in [&api, &ui] {
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
    }
    if api.path() != "/"
        || !matches!(ui.path(), "/classic" | "/app/")
        || ui.path() == "/app/" && ui.origin() != api.origin()
    {
        return None;
    }
    Some((api, ui, instance))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reopen_cannot_follow_foreign_or_credentialed_urls() {
        let mut status = serde_json::json!({"api_url":"http://127.0.0.1:52123","ui_url":"http://127.0.0.1:52124/classic","instance_id":"owner"});
        assert!(owned_addresses(&status).is_some());
        for url in [
            "https://127.0.0.1:52124/classic",
            "http://example.invalid:52124/classic",
            "http://user@127.0.0.1:52124/classic",
            "http://127.0.0.1:52124/app/",
            "http://127.0.0.1:52124/classic?foreign=1",
        ] {
            status["ui_url"] = url.into();
            assert!(owned_addresses(&status).is_none());
        }
    }
}

pub struct Probe {
    client: Client,
    next: Instant,
    failures: HashMap<&'static str, usize>,
}
impl Probe {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(800))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| {
                Failure::new(
                    "PROBE_UNAVAILABLE",
                    "Не удалось проверить локальные службы.",
                )
            })?;
        Ok(Self {
            client,
            next: Instant::now(),
            failures: HashMap::new(),
        })
    }
    pub fn get(&self, url: &str, key: Option<&str>) -> Option<(u16, Value)> {
        let mut request = self.client.get(url);
        if let Some(key) = key {
            request = request.header("api-key", key);
        }
        let response = request.send().ok()?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response.take(65_537).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 65_536 {
            return None;
        }
        Some((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }
    pub fn instance(&self, url: &str, expected: &str) -> bool {
        self.get(url, None)
            .is_some_and(|(status, value)| status == 200 && value["instance_id"] == expected)
    }
    pub fn qdrant(&self, url: &str, key: &str, check_protection: bool) -> bool {
        let endpoint = format!("{url}/collections");
        self.get(&endpoint, Some(key))
            .is_some_and(|(status, _)| status == 200)
            && (!check_protection
                || self
                    .get(&endpoint, None)
                    .is_some_and(|(status, _)| matches!(status, 401 | 403)))
    }
    pub fn failed(
        &mut self,
        api: &str,
        ui: &str,
        instance: &str,
        qdrant: &str,
        key: &str,
    ) -> Option<Value> {
        if Instant::now() < self.next {
            return None;
        }
        let checks = [
            ("API", self.instance(api, instance)),
            ("UI", self.instance(ui, instance)),
            ("Qdrant", self.qdrant(qdrant, key, false)),
        ];
        let mut details = serde_json::Map::new();
        let mut failed = false;
        for (name, healthy) in checks {
            let count = self.failures.entry(name).or_default();
            *count = if healthy { 0 } else { *count + 1 };
            failed |= *count >= 6;
            details.insert(
                name.into(),
                serde_json::json!({"healthy":healthy,"consecutive_failures":count}),
            );
        }
        self.next = Instant::now() + Duration::from_secs(5);
        failed.then(|| serde_json::json!({"reason":"health_probe_failed","services":details}))
    }
}
