use crate::model_http::{self, McpHeaders};
use futures_util::StreamExt;
use reqwest::Url;
use serde_json::Value;
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

pub struct Http {
    url: Url,
    addresses: Vec<IpAddr>,
    session: Option<String>,
    pub protocol: Option<String>,
}
impl Http {
    async fn server_request(&self, request: &Value, deadline: Instant) -> Result<(), &'static str> {
        let response = if request["method"] == "ping" {
            serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":{}})
        } else {
            serde_json::json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Client capability not available"}})
        };
        let mut reply = model_http::mcp(
            self.url.clone(),
            self.addresses.clone(),
            serde_json::to_vec(&response).map_err(|_| "MCP_REQUEST_INVALID")?,
            deadline.saturating_duration_since(Instant::now()),
            McpHeaders {
                session: self.session.clone(),
                protocol: self.protocol.clone(),
                terminate: false,
            },
        )
        .await?;
        if !reply.status.is_success() {
            return Err("MCP_HTTP_FAILED");
        }
        let mut total = 0;
        while let Some(block) = reply.source.next().await {
            total += block?.len();
            if total > 1_000_000 {
                return Err("MCP_RESPONSE_LIMIT");
            }
        }
        Ok(())
    }
    pub async fn close(&mut self) {
        if self.session.is_none() {
            return;
        }
        let _ = model_http::mcp(
            self.url.clone(),
            self.addresses.clone(),
            Vec::new(),
            Duration::from_secs(2),
            McpHeaders {
                session: self.session.take(),
                protocol: self.protocol.clone(),
                terminate: true,
            },
        )
        .await;
    }
    pub async fn new(raw: &str) -> Result<Self, &'static str> {
        let url = super::url(raw)?;
        let host = url
            .host_str()
            .ok_or("MCP_URL_INVALID")?
            .trim_matches(['[', ']']);
        let port = url.port_or_known_default().ok_or("MCP_URL_INVALID")?;
        let resolved = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| "MCP_DNS_FAILED")?
        .map_err(|_| "MCP_DNS_FAILED")?;
        let mut addresses = resolved.map(|v| v.ip()).collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        if addresses.is_empty() || addresses.len() > 64 {
            return Err("MCP_DNS_FAILED");
        }
        if addresses.iter().any(|ip| {
            if url.scheme() == "http" {
                crate::model_runtime::permitted(*ip, "loopback").is_err()
            } else {
                ["loopback", "private_network", "remote"]
                    .iter()
                    .all(|scope| crate::model_runtime::permitted(*ip, scope).is_err())
            }
        }) {
            return Err("MCP_DESTINATION_DENIED");
        }
        Ok(Self {
            url,
            addresses,
            session: None,
            protocol: None,
        })
    }
    pub async fn exchange(
        &mut self,
        request: &Value,
        deadline: Instant,
    ) -> Result<Value, &'static str> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err("MCP_TIMEOUT");
        }
        let data = serde_json::to_vec(request).map_err(|_| "MCP_REQUEST_INVALID")?;
        if data.len() > 256000 {
            return Err("MCP_REQUEST_LIMIT");
        }
        let mut reply = model_http::mcp(
            self.url.clone(),
            self.addresses.clone(),
            data,
            timeout,
            McpHeaders {
                session: self.session.clone(),
                protocol: self.protocol.clone(),
                terminate: false,
            },
        )
        .await?;
        if !reply.status.is_success() {
            return Err("MCP_HTTP_FAILED");
        }
        if let Some(session) = reply.mcp_session {
            if session.is_empty()
                || session.len() > 2048
                || !session.bytes().all(|b| (0x21..=0x7e).contains(&b))
                || self.session.as_ref().is_some_and(|s| *s != session)
            {
                return Err("MCP_SESSION_INVALID");
            }
            self.session = Some(session);
        }
        let expected = request.get("id");
        if expected.is_none() {
            let mut size = 0;
            while let Some(block) = reply.source.next().await {
                size += block?.len();
                if size > 1_000_000 {
                    return Err("MCP_RESPONSE_LIMIT");
                }
            }
            return Ok(Value::Null);
        }
        let sse = reply
            .content_type
            .as_deref()
            .is_some_and(|v| v.split(';').next().unwrap_or("").trim() == "text/event-stream");
        if !sse
            && !reply
                .content_type
                .as_deref()
                .is_some_and(|v| v.split(';').next().unwrap_or("").trim() == "application/json")
        {
            return Err("MCP_CONTENT_TYPE_INVALID");
        }
        let mut bytes = Vec::new();
        let mut total = 0usize;
        while let Some(block) = reply.source.next().await {
            let block = block?;
            total += block.len();
            if total > 1_000_000 {
                return Err("MCP_RESPONSE_LIMIT");
            }
            bytes.extend_from_slice(&block);
            if sse {
                while let Some((end, width)) = separator(&bytes) {
                    let frame = bytes.drain(..end + width).collect::<Vec<_>>();
                    if let Some(value) = event(&frame[..end])? {
                        if value["jsonrpc"] == "2.0"
                            && value["method"].is_string()
                            && value.get("id").is_some()
                        {
                            self.server_request(&value, deadline).await?;
                            continue;
                        }
                        if let Some(value) = super::response(value, expected.unwrap())? {
                            return Ok(value);
                        }
                    }
                }
            }
        }
        if sse {
            return Err("MCP_STREAM_INTERRUPTED");
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| "MCP_RESPONSE_INVALID")?;
        super::response(value, expected.unwrap())?.ok_or("MCP_RESPONSE_INVALID")
    }
}
fn separator(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|v| v == b"\n\n").map(|v| (v, 2));
    let crlf = bytes
        .windows(4)
        .position(|v| v == b"\r\n\r\n")
        .map(|v| (v, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}
fn event(frame: &[u8]) -> Result<Option<Value>, &'static str> {
    let frame = std::str::from_utf8(frame)
        .map_err(|_| "MCP_RESPONSE_INVALID")?
        .trim_start_matches('\u{feff}');
    let data = frame
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|_| "MCP_RESPONSE_INVALID")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_sse_and_crlf_do_not_cut_utf8() {
        let data="event: message\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":1,\"result\":{\"text\":\"Я字\"}}\r\n\r\n".as_bytes();
        let (end, width) = separator(data).unwrap();
        assert_eq!(end + width, data.len());
        assert_eq!(
            event(&data[..end]).unwrap().unwrap()["result"]["text"],
            "Я字"
        );
    }
}
