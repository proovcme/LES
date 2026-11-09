//! Bounded IMAP fetch: write checkpoints only after committing the original.
use super::{Result, BAD};
use crate::Context;
use les_core::catalog::mail;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
const PROTOCOL: &str = "MAIL_IMAP_RESPONSE_INVALID";
const IO: &str = "MAIL_IMAP_CONNECTION_LOST";
const WINDOW: Duration = Duration::from_secs(180);

trait Socket: Read + Write {}
impl<T: Read + Write> Socket for T {}
struct Frame {
    line: String,
    literal: Option<Vec<u8>>,
}
struct Session {
    stream: BufReader<Box<dyn Socket>>,
    tag: u32,
    deadline: Instant,
}
fn quote(value: &str) -> Result<String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(BAD);
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
fn connect(config: &Value) -> Result<Box<dyn Socket>> {
    let host = config["host"].as_str().ok_or(BAD)?;
    let port = config["port"]
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or(BAD)?;
    let tls = config["ssl"] != false;
    let addresses = if tls {
        (host, port)
            .to_socket_addrs()
            .map_err(|_| "MAIL_IMAP_CONNECT_FAILED")?
            .collect::<Vec<_>>()
    } else {
        // A hostname must never turn a plaintext test connection into a remote login.
        let ip = host
            .parse::<IpAddr>()
            .ok()
            .filter(IpAddr::is_loopback)
            .ok_or("MAIL_IMAP_TLS_REQUIRED")?;
        vec![SocketAddr::new(ip, port)]
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = addresses
        .into_iter()
        .find_map(|address| {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(2))).ok()
        })
        .ok_or("MAIL_IMAP_CONNECT_FAILED")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|_| IO)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .map_err(|_| IO)?;
    if !tls {
        return Ok(Box::new(stream));
    }
    // Keep the configured hostname for certificate verification, including after DNS lookup.
    let connector = native_tls::TlsConnector::builder()
        .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
        .build()
        .map_err(|_| "MAIL_IMAP_TLS_FAILED")?;
    connector
        .connect(host, stream)
        .map(|stream| Box::new(stream) as Box<dyn Socket>)
        .map_err(|_| "MAIL_IMAP_TLS_FAILED")
}
impl Session {
    fn open(config: &Value, password: &str) -> Result<Self> {
        let stream = connect(config)?;
        let mut session = Self {
            stream: BufReader::new(stream),
            tag: 0,
            deadline: Instant::now() + WINDOW,
        };
        if !session.line()?.to_ascii_uppercase().starts_with("* OK ") {
            return Err(PROTOCOL);
        }
        let login = quote(config["login"].as_str().ok_or(BAD)?)?;
        let password = quote(password)?;
        session.command(
            &format!("LOGIN {login} {password}"),
            "MAIL_IMAP_LOGIN_FAILED",
            0,
            |_| Ok(()),
        )?;
        Ok(session)
    }
    fn line(&mut self) -> Result<String> {
        if Instant::now() > self.deadline {
            return Err("MAIL_SYNC_TIME_LIMIT");
        }
        let mut bytes = Vec::new();
        self.stream
            .by_ref()
            .take(256 * 1024)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| IO)?;
        if !bytes.ends_with(b"\r\n") {
            return Err(IO);
        }
        bytes.truncate(bytes.len() - 2);
        String::from_utf8(bytes).map_err(|_| PROTOCOL)
    }
    fn command(
        &mut self,
        command: &str,
        error: &'static str,
        literal_limit: usize,
        mut on_frame: impl FnMut(Frame) -> Result<()>,
    ) -> Result<()> {
        self.tag = self.tag.checked_add(1).ok_or(PROTOCOL)?;
        let tag = format!("L{:08X}", self.tag);
        let stream = self.stream.get_mut();
        write!(stream, "{tag} {command}\r\n")
            .and_then(|_| stream.flush())
            .map_err(|_| IO)?;
        let mut remaining = literal_limit;
        for _ in 0..8192 {
            let line = self.line()?;
            if let Some(result) = line.strip_prefix(&tag).and_then(|v| v.strip_prefix(' ')) {
                return if result
                    .split_ascii_whitespace()
                    .next()
                    .is_some_and(|v| v.eq_ignore_ascii_case("OK"))
                {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            if line.eq_ignore_ascii_case("* BYE") || line.to_ascii_uppercase().starts_with("* BYE ")
            {
                return Err(IO);
            }
            let literal =
                if let Some((_, len)) = line.strip_suffix('}').and_then(|v| v.rsplit_once('{')) {
                    let size = len
                        .parse::<usize>()
                        .ok()
                        .filter(|s| *s <= remaining)
                        .ok_or("MAIL_IMAP_LITERAL_LIMIT")?;
                    remaining -= size;
                    let mut bytes = vec![0; size];
                    self.stream.read_exact(&mut bytes).map_err(|_| IO)?;
                    Some(bytes)
                } else {
                    None
                };
            on_frame(Frame { line, literal })?;
        }
        Err(PROTOCOL)
    }
    fn folders(&mut self) -> Result<Vec<Folder>> {
        let mut result = Vec::new();
        self.command(
            "LIST \"\" \"*\"",
            "MAIL_IMAP_LIST_FAILED",
            1024 * 1024,
            |frame| {
                if let Some(folder) = folder(frame)? {
                    result.push(folder);
                }
                if result.len() > 2048 {
                    return Err("MAIL_IMAP_FOLDER_LIMIT");
                }
                Ok(())
            },
        )?;
        Ok(result)
    }
    fn examine(&mut self, name: &str) -> Result<u32> {
        let mut validity = None;
        self.command(
            &format!("EXAMINE {}", quote(name)?),
            "MAIL_IMAP_FOLDER_FAILED",
            0,
            |frame| {
                let line = frame.line.to_ascii_uppercase();
                if let Some((_, raw)) = line.split_once("[UIDVALIDITY ") {
                    let found = raw
                        .split(']')
                        .next()
                        .and_then(|v| v.parse::<u32>().ok())
                        .filter(|v| *v > 0)
                        .ok_or(PROTOCOL)?;
                    if validity.replace(found).is_some_and(|v| v != found) {
                        return Err(PROTOCOL);
                    }
                }
                Ok(())
            },
        )?;
        validity.ok_or(PROTOCOL)
    }
    fn search(&mut self, start: u32) -> Result<Vec<u32>> {
        let mut found = false;
        let mut result = Vec::new();
        self.command(
            &format!("UID SEARCH UID {start}:*"),
            "MAIL_IMAP_SEARCH_FAILED",
            0,
            |frame| {
                let upper = frame.line.to_ascii_uppercase();
                if upper == "* SEARCH" || upper.starts_with("* SEARCH ") {
                    found = true;
                    for id in upper[8..].split_ascii_whitespace() {
                        let id = id.parse::<u32>().map_err(|_| PROTOCOL)?;
                        if id >= start {
                            result.push(id);
                        }
                        if result.len() > 100_000 {
                            return Err("MAIL_IMAP_SEARCH_LIMIT");
                        }
                    }
                }
                Ok(())
            },
        )?;
        if !found {
            return Err(PROTOCOL);
        }
        result.sort_unstable();
        result.dedup();
        Ok(result)
    }
    fn fetch(&mut self, uid: u32) -> Result<Vec<u8>> {
        let mut bytes = None;
        let mut response = None;
        let mut continuation = false;
        self.command(
            &format!("UID FETCH {uid} (UID BODY.PEEK[])"),
            "MAIL_IMAP_FETCH_FAILED",
            mail::MAX_MESSAGE,
            |frame| {
                let line = frame.line.to_ascii_uppercase();
                let attrs = if let Some(body) = frame.literal {
                    let (seq, attrs) = line
                        .strip_prefix("* ")
                        .and_then(|v| v.split_once(" FETCH ("))
                        .ok_or(PROTOCOL)?;
                    if seq.parse::<u32>().ok().filter(|v| *v > 0).is_none()
                        || bytes.is_some()
                        || !attrs.contains("BODY[] {")
                    {
                        return Err(PROTOCOL);
                    }
                    bytes = Some(body);
                    continuation = true;
                    attrs
                } else if continuation {
                    if !line.ends_with(')') || line.starts_with('*') {
                        return Err(PROTOCOL);
                    }
                    continuation = false;
                    line.trim_start()
                } else {
                    return Ok(());
                };
                if let Some(rest) = attrs
                    .strip_prefix("UID ")
                    .or_else(|| attrs.split_once(" UID ").map(|(_, r)| r))
                {
                    let value = rest
                        .split(|c: char| !c.is_ascii_digit())
                        .next()
                        .and_then(|v| v.parse::<u32>().ok())
                        .ok_or(PROTOCOL)?;
                    if value != uid || response.replace(value).is_some() {
                        return Err(PROTOCOL);
                    }
                }
                Ok(())
            },
        )?;
        if continuation || response != Some(uid) {
            return Err(PROTOCOL);
        }
        bytes.filter(|v| !v.is_empty()).ok_or(PROTOCOL)
    }
}
#[derive(Debug)]
struct Folder {
    wire: String,
    label: String,
}
fn unquote(value: &str) -> Result<(String, &str)> {
    if !value.starts_with('"') {
        let (v, r) = value.split_once(' ').unwrap_or((value, ""));
        return Ok((v.to_owned(), r.trim_start()));
    }
    let mut escaped = false;
    let mut out = String::new();
    for (pos, c) in value.char_indices().skip(1) {
        if escaped {
            if !matches!(c, '"' | '\\') {
                return Err(PROTOCOL);
            }
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Ok((out, value[pos + 1..].trim_start()));
        } else {
            out.push(c);
        }
    }
    Err(PROTOCOL)
}
fn display(wire: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = wire;
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";
    while let Some((first, tail)) = rest.split_once('&') {
        out.push_str(first);
        let Some((encoded, next)) = tail.split_once('-') else {
            return Ok(wire.to_owned());
        };
        rest = next;
        if encoded.is_empty() {
            out.push('&');
            continue;
        }
        let mut bytes = Vec::new();
        let (mut accumulator, mut bits) = (0u32, 0);
        for byte in encoded.bytes() {
            let Some(pos) = ABC.iter().position(|v| *v == byte) else {
                return Ok(wire.to_owned());
            };
            accumulator = (accumulator << 6) | pos as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                bytes.push((accumulator >> bits) as u8);
            }
        }
        if bytes.len() % 2 != 0 || bits >= 6 || accumulator & ((1 << bits) - 1) != 0 {
            return Err(PROTOCOL);
        }
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|v| u16::from_be_bytes([v[0], v[1]]))
            .collect();
        out.push_str(&String::from_utf16(&units).map_err(|_| PROTOCOL)?);
    }
    out.push_str(rest);
    Ok(out)
}
fn folder(frame: Frame) -> Result<Option<Folder>> {
    let mut list = frame
        .line
        .get(0..7)
        .is_some_and(|v| v.eq_ignore_ascii_case("* LIST "));
    if !list {
        return Ok(None);
    }
    let (flags, tail) = frame.line[7..]
        .strip_prefix('(')
        .and_then(|v| v.split_once(')'))
        .ok_or(PROTOCOL)?;
    list = !flags.split_ascii_whitespace().any(|v| {
        [
            "\\Noselect",
            "\\NonExistent",
            "\\Trash",
            "\\Junk",
            "\\Drafts",
        ]
        .iter()
        .any(|f| v.eq_ignore_ascii_case(f))
    });
    if !list {
        return Ok(None);
    }
    let (_, mailbox) = unquote(tail.trim_start())?;
    let wire = if let Some(bytes) = frame.literal {
        String::from_utf8(bytes).map_err(|_| PROTOCOL)?
    } else {
        unquote(mailbox)?.0
    };
    if wire.is_empty() || wire.len() > 2048 || wire.chars().any(char::is_control) {
        return Err(PROTOCOL);
    }
    let label = display(&wire)?;
    if label.chars().any(char::is_control) {
        return Err(PROTOCOL);
    }
    Ok(Some(Folder { wire, label }))
}
fn lease(state: &Path) -> Result<File> {
    let data = state.join("data");
    fs::create_dir_all(&data).map_err(|_| "MAIL_REGISTRY_UNAVAILABLE")?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("native-mail-sync.lock"))
        .map_err(|_| "MAIL_REGISTRY_UNAVAILABLE")?;
    lock.try_lock().map_err(|_| "MAIL_SYNC_BUSY")?;
    Ok(lock)
}
pub fn test(state: &Path, id: &str) -> Result<Value> {
    let _lease = lease(state)?;
    let a = mail::account(state, id)?;
    let mut result = json!({"status":"ready","kind":a["kind"],"dataset_id":a["dataset_id"]});
    if a["kind"] == "imap" {
        let password = mail::secret(id)?;
        let mut session = Session::open(&a["config"], &password)?;
        result["folders"] = json!(session.folders()?.len());
    }
    Ok(result)
}
pub(super) fn update_account(state: &Path, id: &str, input: &Value) -> Result<Value> {
    // Read the connection settings and its credential under the same lease as sync.
    let _lock = lease(state)?;
    mail::update(state, id, input)
}
fn run(
    context: &Context,
    id: &str,
    config: &Value,
    password: &str,
    input: &Value,
    limit: usize,
) -> Result<usize> {
    let state = &context.state_root;
    let mut session = Session::open(config, password)?;
    let folders = session.folders()?;
    let configured = config["folders"].as_array().ok_or(BAD)?;
    let mut downloaded = 0;
    for folder in folders {
        if !configured.iter().any(|v| {
            v.as_str().is_some_and(|s| {
                s == "*"
                    || s == folder.label
                    || s == folder.wire
                    || (s.eq_ignore_ascii_case("INBOX")
                        && folder.wire.eq_ignore_ascii_case("INBOX"))
            })
        }) {
            continue;
        }
        let validity = session.examine(&folder.wire)?;
        let old = mail::cursor(state, id, &folder.wire)?;
        if old["enabled"] == 0 {
            continue;
        }
        let restart = input["mode"] == "full" && old["backfill_complete"] == 1;
        let mut last = if !restart && old["uid_validity"] == validity.to_string() {
            old["last_uid"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or_default()
        } else {
            0
        };
        let Some(start) = last.checked_add(1) else {
            continue;
        };
        let uids = session.search(start)?;
        let mut complete = true;
        for uid in uids {
            if downloaded >= limit {
                complete = false;
                break;
            }
            let raw = session.fetch(uid)?;
            mail::register(
                state,
                id,
                "eml",
                &raw,
                &json!({"native_id":format!("{}:{validity}:{uid}",folder.wire),
                "folder_id":folder.wire,"folder_path":folder.label,"parse":input["parse"].as_bool().unwrap_or(true)}),
            )?;
            last = uid;
            mail::folder_checkpoint(
                state,
                id,
                &folder.wire,
                &folder.label,
                validity,
                last,
                false,
            )?;
            crate::intake::wake(context);
            downloaded += 1;
        }
        mail::folder_checkpoint(
            state,
            id,
            &folder.wire,
            &folder.label,
            validity,
            last,
            complete,
        )?;
        if downloaded >= limit {
            break;
        }
    }
    Ok(downloaded)
}
pub fn start(context: Arc<Context>, id: String, input: Value) -> Result<Value> {
    let limit = input["max_messages"].as_u64().unwrap_or(100);
    if !matches!(
        input["mode"].as_str().unwrap_or("incremental"),
        "incremental" | "full"
    ) || !(1..=2000).contains(&limit)
    {
        return Err(BAD);
    }
    let lock = lease(&context.state_root)?;
    mail::recover_sync(&context.state_root)?;
    let a = begin(&context.state_root, &id)?;
    let result = json!({"status":"running","account_id":id,"dataset_id":a["dataset_id"]});
    std::thread::spawn(move || {
        let _lock = lock;
        finish(&context, &id, &a, &input, limit as usize);
    });
    Ok(result)
}

fn begin(state: &Path, id: &str) -> Result<Value> {
    let a = mail::account(state, id)?;
    if a["kind"] != "imap" {
        return Err(BAD);
    }
    if a["enabled"] != true {
        return Err("MAIL_ACCOUNT_DISABLED");
    }
    if a["secret_set"] != true {
        return Err("MAIL_SECRET_MISSING");
    }
    mail::sync_status(state, id, "running", "")?;
    Ok(a)
}

fn record_outcome(context: &Context, id: &str, outcome: &Result<usize>) {
    let (status, detail) = match outcome {
        Ok(_) => ("idle", ""),
        Err(code) => ("error", *code),
    };
    if let Err(code) = mail::sync_status(&context.state_root, id, status, detail) {
        eprintln!(
            "{}",
            json!({"event":"native_mail_checkpoint_failed","code":code})
        );
    }
    eprintln!(
        "{}",
        json!({"event":"native_mail_sync","status":status,"code":detail,"downloaded":outcome.as_ref().ok()})
    );
}

fn finish(context: &Context, id: &str, a: &Value, input: &Value, limit: usize) {
    let outcome = mail::secret(id)
        .and_then(|password| run(context, id, &a["config"], &password, input, limit));
    record_outcome(context, id, &outcome);
}

pub(super) fn import_once(context: Arc<Context>, input: Value) -> Result<Value> {
    use crate::index_batch::configuration::Settings;
    let settings = Settings::load(&context.state_root).map_err(|_| BAD)?;
    let text = |field: &str, env: &str| {
        input[field]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| settings.get(env, ""))
    };
    let limit = input["max_messages"].as_u64().unwrap_or(25);
    if !(1..=200).contains(&limit) {
        return Err(BAD);
    }
    let password = text("password", "MAIL_IMAP_PASSWORD").to_owned();
    if password.is_empty() || password.len() > 2560 || password.chars().any(char::is_control) {
        return Err(BAD);
    }
    let port = input["port"]
        .as_u64()
        .or_else(|| settings.get("MAIL_IMAP_PORT", "993").parse().ok())
        .ok_or(BAD)?;
    let ssl = input["ssl"].as_bool().unwrap_or_else(|| {
        !matches!(
            settings
                .get("MAIL_IMAP_SSL", "true")
                .to_ascii_lowercase()
                .as_str(),
            "false" | "0" | "off" | "no"
        )
    });
    let folders = input.get("folders").cloned().unwrap_or_else(|| {
        json!(settings
            .get("MAIL_IMAP_FOLDERS", "INBOX")
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect::<Vec<_>>())
    });
    let config = mail::imap_config(
        &json!({"host":text("host","MAIL_IMAP_HOST"),"login":text("login","MAIL_IMAP_LOGIN"),
        "port":port,"ssl":ssl,"folders":folders}),
    )?;
    let key = les_core::sha256_hex(
        format!("{}:{}:{}", config["host"], config["port"], config["login"]).as_bytes(),
    );
    let lock = lease(&context.state_root)?;
    mail::recover_sync(&context.state_root)?;
    let account = mail::create(
        &context.state_root,
        &context.database,
        &json!({"kind":"archive","label":"Импорт IMAP","native_account_id":format!("imap:{key}")}),
        true,
    )?;
    let id = account["id"].as_str().ok_or(BAD)?.to_owned();
    if account["enabled"] != true {
        return Err("MAIL_ACCOUNT_DISABLED");
    }
    let parse = input["parse"].as_bool().unwrap_or(false);
    let options = json!({"parse":parse});
    mail::sync_status(&context.state_root, &id, "running", "")?;
    let mut response = json!({"status":"running","component":"Е.Ж.И.К.","account_id":id,
        "dataset_id":account["dataset_id"],"dataset_name":account["dataset_name"]});
    if input["background"] == true {
        std::thread::spawn(move || {
            let _lock = lock;
            let result = run(&context, &id, &config, &password, &options, limit as usize);
            record_outcome(&context, &id, &result);
        });
    } else {
        let _lock = lock;
        let result = run(&context, &id, &config, &password, &options, limit as usize);
        record_outcome(&context, &id, &result);
        let count = result?;
        response["status"] = json!(if count > 0 {
            "registered"
        } else {
            "no_new_mail"
        });
        response["files"] = json!(count);
        response["parse_started"] = json!(parse && count > 0);
    }
    Ok(response)
}

pub(super) fn poll(context: &Context) -> Result<()> {
    let state = &context.state_root;
    if !state.join("data/mail_registry.db").is_file() {
        return Ok(());
    }
    for id in mail::enabled_imap_accounts(state)? {
        let _lock = lease(state)?;
        mail::recover_sync(state)?;
        match begin(state, &id) {
            Ok(account) => finish(context, &id, &account, &Value::Null, 200),
            Err("MAIL_ACCOUNT_DISABLED" | "MAIL_SECRET_MISSING") => continue,
            Err(code) => return Err(code),
        }
    }
    Ok(())
}
pub fn recover(state: &Path) {
    if state.join("data/mail_registry.db").is_file() {
        if let Ok(_lock) = lease(state) {
            let _ = mail::recover_sync(state);
        }
    }
}
