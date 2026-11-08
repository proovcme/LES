//! Run a bounded embedding request through the system TLS client with pinned peers.
use crate::ollama_embed::{self, Format, Limits};
use axum::http::{HeaderMap, StatusCode};
use reqwest::Url;
use std::{
    io::{Read, Write},
    net::IpAddr,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const ADDRESSES: &str = "x-les-embed-addresses";
const TARGET: &str = "MODEL_BRIDGE_TARGET_REJECTED";
const FAILED: &str = "UPSTREAM_UNREACHABLE";
const LIMIT: &str = "UPSTREAM_RESPONSE_TOO_LARGE";

pub struct Call {
    url: Url,
    addresses: Vec<IpAddr>,
    authorization: Option<String>,
    timeout: Duration,
    limits: Limits,
    format: Format,
}

fn allowed(address: IpAddr, locality: &str) -> bool {
    if address.is_unspecified() || address.is_multicast() {
        return false;
    }
    let (private, link_local, reserved) = match address {
        IpAddr::V4(ip) => (
            ip.is_private(),
            ip.is_link_local(),
            ip.is_broadcast()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 240
                || ip.octets() == [100, 100, 100, 200],
        ),
        IpAddr::V6(ip) => (
            ip.is_unique_local(),
            ip.is_unicast_link_local(),
            ip.to_ipv4_mapped().is_some(),
        ),
    };
    !link_local
        && !reserved
        && match locality {
            "loopback" => address.is_loopback(),
            "private_network" => private && !address.is_loopback(),
            "remote" => !private && !address.is_loopback(),
            _ => false,
        }
}

impl Call {
    pub fn new(url: Url, headers: &HeaderMap, ports: [u16; 2]) -> Result<Self, &'static str> {
        let get = |key: &str| {
            headers
                .get(key)
                .and_then(|value| value.to_str().ok())
                .ok_or(TARGET)
        };
        let locality = get("x-les-embed-locality")?;
        let port = url.port_or_known_default().ok_or(TARGET)?;
        if !matches!(url.scheme(), "http" | "https")
            || locality == "remote" && url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || ports.contains(&port) && locality == "loopback"
            || headers.contains_key(ollama_embed::LIMITS_HEADER)
                && headers.contains_key("x-les-chat-limits")
        {
            return Err(TARGET);
        }
        let format = match url.path() {
            "/api/embed" => Format::Ollama,
            path if path.ends_with("/embeddings") => Format::OpenAi,
            _ => return Err(TARGET),
        };
        let limits = Limits::parse(get(ollama_embed::LIMITS_HEADER)?).ok_or(TARGET)?;
        let addresses = get(ADDRESSES)?
            .split(',')
            .take(17)
            .map(str::parse::<IpAddr>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TARGET)?;
        if addresses.is_empty()
            || addresses.len() > 16
            || !addresses.iter().all(|ip| allowed(*ip, locality))
        {
            return Err(TARGET);
        }
        if let Ok(ip) = url
            .host_str()
            .ok_or(TARGET)?
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
        {
            if addresses.as_slice() != [ip] {
                return Err(TARGET);
            }
        }
        let authorization = headers
            .get("authorization")
            .map(|value| {
                value
                    .to_str()
                    .ok()
                    .filter(|value| {
                        value.len() <= 4096
                            && value.starts_with("Bearer ")
                            && !value.chars().any(char::is_control)
                    })
                    .map(str::to_owned)
                    .ok_or(TARGET)
            })
            .transpose()?;
        let seconds = get("x-les-embed-deadline")?
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=600).contains(n))
            .ok_or(TARGET)?;
        Ok(Self {
            url,
            addresses,
            authorization,
            timeout: Duration::from_secs(seconds),
            limits,
            format,
        })
    }

    pub fn send(self, data: &[u8]) -> Result<(StatusCode, Vec<u8>), &'static str> {
        let json = std::str::from_utf8(data)
            .ok()
            .filter(|data| data.trim_start().starts_with('{'))
            .ok_or(TARGET)?;
        let mut config = String::new();
        option(&mut config, "data-binary", json)?;
        if let Some(secret) = &self.authorization {
            option(&mut config, "header", &format!("Authorization: {secret}"))?;
        }
        let host = self.url.host_str().ok_or(TARGET)?;
        let mut command = Command::new(executable().ok_or("UPSTREAM_TLS_UNAVAILABLE")?);
        command
            .args([
                "--disable",
                "--silent",
                "--globoff",
                "--noproxy",
                "*",
                "--proto",
                "=http,https",
                "--connect-timeout",
                "5",
                "--max-time",
            ])
            .arg(self.timeout.as_secs().to_string())
            .arg("--max-filesize")
            .arg(self.limits.bytes.to_string())
            .args([
                "--header",
                "Content-Type: application/json",
                "--header",
                "Accept: application/json",
                "--write-out",
                "\n%{http_code}\n%{remote_ip}",
                "--config",
                "-",
                "--url",
            ])
            .arg(self.url.as_str());
        if host.trim_matches(['[', ']']).parse::<IpAddr>().is_err() {
            let peers = self
                .addresses
                .iter()
                .map(|address| match address {
                    IpAddr::V4(ip) => ip.to_string(),
                    IpAddr::V6(ip) => format!("[{ip}]"),
                })
                .collect::<Vec<_>>()
                .join(",");
            let port = self.url.port_or_known_default().ok_or(TARGET)?;
            command
                .arg("--resolve")
                .arg(format!("{host}:{port}:{peers}"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| FAILED)?;
        let mut input = child.stdin.take().expect("piped curl stdin");
        let writer = thread::spawn(move || input.write_all(config.as_bytes()));
        let stream = child.stdout.take().expect("piped curl stdout");
        let cap = self.limits.bytes + 64;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.take(cap as u64 + 1).read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        });
        let deadline = Instant::now() + self.timeout + Duration::from_secs(2);
        let result = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().ok_or(FAILED),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(if result.is_err() {
                        FAILED
                    } else {
                        "UPSTREAM_TIMEOUT"
                    });
                }
            }
        };
        let input_status = writer.join().map_err(|_| FAILED)?;
        let bytes = reader.join().map_err(|_| FAILED)?.map_err(|_| FAILED)?;
        let exit = result?;
        if exit == 63 || bytes.len() > cap {
            return Err(LIMIT);
        }
        if exit == 28 {
            return Err("UPSTREAM_TIMEOUT");
        }
        if exit != 0 || input_status.is_err() {
            return Err(FAILED);
        }
        let mut parts = bytes.rsplitn(3, |byte| *byte == b'\n');
        let peer = std::str::from_utf8(parts.next().ok_or(FAILED)?)
            .ok()
            .and_then(|value| value.parse::<IpAddr>().ok())
            .ok_or("CONNECTED_PEER_UNAVAILABLE")?;
        if !self.addresses.contains(&peer) {
            return Err("CONNECTED_PEER_MISMATCH");
        }
        let status = StatusCode::from_bytes(parts.next().ok_or(FAILED)?).map_err(|_| FAILED)?;
        let body = parts.next().ok_or(FAILED)?;
        if body.len() > self.limits.bytes {
            return Err(LIMIT);
        }
        if status.is_redirection() {
            return Err("UPSTREAM_REDIRECT_REJECTED");
        }
        if status.is_success() {
            return Ok((
                status,
                ollama_embed::encode(body, self.limits.count, self.format)?,
            ));
        }
        Ok((status, Vec::new()))
    }
}

pub(crate) fn option(config: &mut String, name: &str, text: &str) -> Result<(), &'static str> {
    config.push_str(name);
    config.push_str(" = \"");
    for character in text.chars() {
        match character {
            '\\' => config.push_str("\\\\"),
            '"' => config.push_str("\\\""),
            '\n' => config.push_str("\\n"),
            '\r' => config.push_str("\\r"),
            '\t' => config.push_str("\\t"),
            other if other.is_control() => return Err(TARGET),
            other => config.push(other),
        }
    }
    config.push_str("\"\n");
    Ok(())
}

pub(crate) fn executable() -> Option<PathBuf> {
    #[cfg(windows)]
    let path = PathBuf::from(std::env::var_os("SystemRoot")?).join("System32/curl.exe");
    #[cfg(not(windows))]
    let path = PathBuf::from("/usr/bin/curl");
    path.is_file().then_some(path)
}
