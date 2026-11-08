//! Resolve an exact saved model revision without calling the retained application server.
use reqwest::Url;
use serde_json::{json, Value};
use std::{
    net::{IpAddr, ToSocketAddrs},
    path::Path,
};

pub struct Endpoint {
    pub url: Url,
    pub addresses: Vec<IpAddr>,
    pub locality: String,
}
pub struct Resolved {
    pub revision: Value,
    pub snapshot: Value,
    pub profile: Value,
    pub endpoint: Endpoint,
    pub authorization: Option<String>,
}

fn in_ranges(address: IpAddr, ranges: &[&str]) -> bool {
    les_core::trust::ip_in_networks(
        &address.to_string(),
        &ranges.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
    )
}
fn private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(_) => {
            in_ranges(
                address,
                &[
                    "0.0.0.0/8",
                    "10.0.0.0/8",
                    "127.0.0.0/8",
                    "169.254.0.0/16",
                    "172.16.0.0/12",
                    "192.0.0.0/24",
                    "192.0.2.0/24",
                    "192.168.0.0/16",
                    "198.18.0.0/15",
                    "198.51.100.0/24",
                    "203.0.113.0/24",
                    "240.0.0.0/4",
                ],
            ) && !in_ranges(address, &["192.0.0.9/32", "192.0.0.10/32"])
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                private(IpAddr::V4(ip))
            } else {
                in_ranges(
                    address,
                    &[
                        "::1/128",
                        "::/128",
                        "64:ff9b:1::/48",
                        "100::/64",
                        "2001::/23",
                        "2001:db8::/32",
                        "2002::/16",
                        "3fff::/20",
                        "fc00::/7",
                        "fe80::/10",
                    ],
                ) && !in_ranges(
                    address,
                    &[
                        "2001:1::1/128",
                        "2001:1::2/128",
                        "2001:3::/32",
                        "2001:4:112::/48",
                        "2001:20::/28",
                        "2001:30::/28",
                    ],
                )
            }
        }
    }
}
pub fn permitted(address: IpAddr, locality: &str) -> Result<(), &'static str> {
    let forbidden = address.is_unspecified()
        || address.is_multicast()
        || match address {
            IpAddr::V4(ip) => ip.is_link_local() || ip.octets() == [100, 100, 100, 200],
            IpAddr::V6(ip) => {
                ip.is_unicast_link_local()
                    || ip == "fd00:ec2::254".parse::<std::net::Ipv6Addr>().unwrap()
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|mapped| permitted(IpAddr::V4(mapped), locality).is_err())
            }
        };
    if forbidden {
        return Err("FORBIDDEN_DESTINATION");
    }
    let valid = match locality {
        "loopback" => address.is_loopback(),
        "private_network" => private(address) && !address.is_loopback(),
        "remote" => !private(address) && !address.is_loopback(),
        _ => return Err("LOCALITY_INVALID"),
    };
    if valid {
        Ok(())
    } else {
        Err("LOCALITY_ADDRESS_MISMATCH")
    }
}
pub fn validate(
    base: &str,
    locality: &str,
    allow_private_http: bool,
) -> Result<Endpoint, &'static str> {
    validate_with(base, locality, allow_private_http, |host, port| {
        (host, port)
            .to_socket_addrs()
            .map(|v| v.map(|v| v.ip()).collect())
            .map_err(|_| "ENDPOINT_DNS_UNAVAILABLE")
    })
}
fn validate_with(
    base: &str,
    locality: &str,
    allow_private_http: bool,
    resolver: impl FnOnce(&str, u16) -> Result<Vec<IpAddr>, &'static str>,
) -> Result<Endpoint, &'static str> {
    let base = base.trim();
    let scheme = base.split(':').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err("ENDPOINT_SCHEME_INVALID");
    }
    if base.contains('\\') || base.chars().any(char::is_control) {
        return Err("ENDPOINT_URL_INVALID");
    }
    let authority = base
        .split_once("://")
        .map(|(_, v)| v.split(['/', '?', '#']).next().unwrap_or(""))
        .ok_or("ENDPOINT_URL_INVALID")?;
    if authority.contains('@') {
        return Err("ENDPOINT_CREDENTIALS_FORBIDDEN");
    }
    let mut url = Url::parse(base).map_err(|_| "ENDPOINT_URL_INVALID")?;
    if url.query().is_some_and(|v| !v.is_empty()) {
        return Err("ENDPOINT_QUERY_FORBIDDEN");
    }
    if url.fragment().is_some_and(|v| !v.is_empty()) {
        return Err("ENDPOINT_FRAGMENT_FORBIDDEN");
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = base
        .split_once("://")
        .unwrap()
        .1
        .find('/')
        .map(|n| &base.split_once("://").unwrap().1[n..])
        .unwrap_or("");
    if path.split(['/', '?', '#']).any(|v| {
        percent_encoding::percent_decode_str(v)
            .decode_utf8()
            .is_ok_and(|s| s == "..")
    }) {
        return Err("ENDPOINT_PATH_INVALID");
    }
    let host = url
        .host_str()
        .filter(|v| !v.is_empty())
        .ok_or("ENDPOINT_HOST_REQUIRED")?
        .trim_matches(['[', ']']);
    if locality == "remote" && scheme != "https" {
        return Err("REMOTE_HTTPS_REQUIRED");
    }
    if locality == "private_network" && scheme == "http" && !allow_private_http {
        return Err("PRIVATE_HTTP_NOT_ALLOWED");
    }
    let port = url
        .port_or_known_default()
        .filter(|v| *v != 0)
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let mut addresses = resolver(host, port)?;
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err("ENDPOINT_DNS_EMPTY");
    }
    for address in &addresses {
        permitted(*address, locality)?;
    }
    Ok(Endpoint {
        url,
        addresses,
        locality: locality.into(),
    })
}
pub fn resolve(execution: &Value, role: &str, env_path: &Path) -> Result<Resolved, String> {
    if execution.is_null() {
        return Err(format!("ROLE_BINDING_MISSING: {role}"));
    }
    if let Some(code) = execution["missing"].as_str() {
        return Err(code.into());
    }
    if execution["enabled"] != true {
        return Err("CONNECTION_DISABLED".into());
    }
    let revision = execution["revision"].clone();
    let secret =
        les_core::model_secrets::resolve(env_path, revision["secret_ref"].as_str(), |key| {
            std::env::var(key).ok()
        })
        .map_err(|code| {
            if code == "SECRET_MISSING" {
                "CONNECTION_SECRET_MISSING".into()
            } else {
                code.to_string()
            }
        })?;
    let snapshot = &execution["snapshot"];
    if snapshot.is_null() {
        return Err("CAPABILITY_SNAPSHOT_MISSING".into());
    }
    let observations: Value = serde_json::from_str(
        snapshot["observations_json"]
            .as_str()
            .ok_or("CAPABILITY_SNAPSHOT_INVALID")?,
    )
    .map_err(|_| "CAPABILITY_SNAPSHOT_INVALID")?;
    let options: Value = serde_json::from_str(
        snapshot["transport_options_json"]
            .as_str()
            .ok_or("CAPABILITY_SNAPSHOT_INVALID")?,
    )
    .map_err(|_| "CAPABILITY_SNAPSHOT_INVALID")?;
    let snapshot = json!({"snapshot_id":snapshot["snapshot_id"],"connection_revision_id":snapshot["connection_revision_id"],"observations":observations,"observed_at":snapshot["observed_at"],"expires_at":snapshot["expires_at"],"transport_options":options});
    let capability = match role {
        "answer" | "local_fallback" => "chat_completions",
        "embeddings" => "embeddings",
        _ => return Err("ROLE_INVALID".into()),
    };
    les_core::model_capabilities::require(&snapshot, &[capability.into()], None)?;
    let endpoint = validate(
        revision["base_url"]
            .as_str()
            .ok_or("ENDPOINT_URL_INVALID")?,
        revision["locality"].as_str().ok_or("LOCALITY_INVALID")?,
        true,
    )?;
    let observed = options["observed_context_tokens"]
        .as_str()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0);
    let operator = revision["requested_context_tokens"]
        .as_u64()
        .filter(|v| *v > 0)
        .map(|v| json!({"input_tokens":v}))
        .unwrap_or(Value::Null);
    let profile = les_core::model_profile::resolve(
        &json!({"model_id":revision["model_id"],"context_tokens":observed,"observed":observed.is_some()}),
        &operator,
        &Value::Null,
    )?;
    Ok(Resolved {
        revision,
        snapshot,
        profile,
        endpoint,
        authorization: secret.map(|s| format!("Bearer {s}")),
    })
}
impl Resolved {
    pub async fn chat(
        &self,
        request: &Value,
        streaming: bool,
        timeout: std::time::Duration,
        ports: [u16; 2],
    ) -> Result<axum::body::Body, String> {
        let mut required = vec!["chat_completions".to_string()];
        if request["tools"].as_array().is_some_and(|v| !v.is_empty()) {
            required.push("tools".into());
        }
        if !request["response_format"].is_null() {
            required.push("structured_output".into());
        }
        let native = self.snapshot["transport_options"]["chat_protocol"] == "native_chat_v1";
        if streaming && !native {
            required.push("streaming".into());
        }
        les_core::model_capabilities::require(&self.snapshot, &required, None)?;
        let mut url = self.endpoint.url.clone();
        let base = url.path().trim_end_matches('/');
        let path = if native {
            let base = base
                .strip_suffix("/api/v1")
                .or_else(|| base.strip_suffix("/v1"))
                .unwrap_or(base);
            format!("{base}/api/chat")
        } else {
            let root = if base.ends_with("/v1") {
                base.to_string()
            } else {
                format!("{base}/v1")
            };
            format!("{root}/chat/completions")
        };
        url.set_path(&path);
        if self.endpoint.locality == "loopback"
            && url
                .port_or_known_default()
                .is_some_and(|v| ports.contains(&v))
        {
            return Err("MODEL_BRIDGE_TARGET_REJECTED".into());
        }
        let connection = json!({"model_id":self.revision["model_id"],"locality":self.endpoint.locality,
            "input_token_limit":self.profile["input_token_limit"],"max_output_field":self.snapshot["transport_options"].get("max_output_field").cloned().unwrap_or(json!("max_tokens"))});
        let body = les_core::model_request::prepare(&connection, request, native, streaming)?;
        let tokens = request["max_output_tokens"]
            .as_u64()
            .ok_or("MODEL_OUTPUT_LIMIT_INVALID")?;
        let reasoning = request["reasoning_enabled"]
            .as_bool()
            .ok_or("MODEL_REASONING_INVALID")?;
        let limits =
            crate::ollama_chat::Limits::parse(&format!("{tokens}/1048576/{}", u8::from(reasoning)))
                .ok_or("MODEL_OUTPUT_LIMIT_INVALID")?;
        let reply = crate::model_http::open(
            url,
            self.endpoint.addresses.clone(),
            self.authorization.clone(),
            body.to_string().into_bytes(),
            timeout,
            false,
        )
        .await?;
        if !reply.status.is_success() {
            return Err(format!("UPSTREAM_HTTP_ERROR: {}", reply.status.as_u16()));
        }
        Ok(if native {
            crate::ollama_chat::from_source(reply.source, limits)
        } else {
            crate::openai_chat::from_source(reply.source, limits, streaming)
        })
    }
    pub fn public(&self) -> Value {
        let r = &self.revision;
        json!({"connection_id":r["connection_id"],"revision_id":r["revision_id"],"display_name":r["display_name"],"model_id":r["model_id"],"locality":r["locality"],"fallback_used":false,"preset_id":self.profile["preset_id"],"input_token_limit":self.profile["input_token_limit"]})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_addresses_must_match_locality_and_metadata_is_forbidden() {
        let result = validate_with("HTTP://LOCALHOST:1919/v1/", "loopback", false, |_, _| {
            Ok(vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()])
        });
        assert!(result.is_ok());
        assert!(
            validate_with("http://model.local/v1", "loopback", false, |_, _| Ok(vec![
                "127.0.0.1".parse().unwrap(),
                "10.0.0.8".parse().unwrap()
            ]))
            .is_err()
        );
        for ip in [
            "169.254.169.254",
            "100.100.100.200",
            "fd00:ec2::254",
            "::",
            "224.0.0.1",
        ] {
            assert_eq!(
                permitted(ip.parse().unwrap(), "remote"),
                Err("FORBIDDEN_DESTINATION")
            );
        }
    }
    #[test]
    fn unsafe_urls_are_rejected_before_dns() {
        for url in [
            "https://u:p@model.local/v1",
            "https://model.local/v1?key=x",
            "https://model.local/v1#x",
            "https://model.local/a/../v1",
            "https://model.local/%2e%2e/v1",
        ] {
            assert!(validate_with(url, "remote", false, |_, _| panic!(
                "Unsafe URL reached DNS"
            ))
            .is_err());
        }
    }
}
