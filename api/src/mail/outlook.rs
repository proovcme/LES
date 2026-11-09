//! The Classic Outlook COM collector remains read-only; Rust owns invocation and import.
use super::{Result, BAD};
use crate::Context;
#[cfg(windows)]
use std::{fs, path::PathBuf, process::Command};

#[cfg(windows)]
fn executable(context: &Context) -> Result<PathBuf> {
    let path = std::env::var_os("LES_OUTLOOK_COLLECTOR_EXE")
        .map(PathBuf::from)
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            [
                exe.parent()?.join("mail/LesLightMailPoller.exe"),
                exe.parent()?.join("../mail/LesLightMailPoller.exe"),
                context.state_root.join("bin/LesLightMailPoller.exe"),
            ]
            .into_iter()
            .find(|p| p.is_file())
        })
        .ok_or("MAIL_OUTLOOK_COLLECTOR_UNAVAILABLE")?;
    if !path.is_absolute() || !path.is_file() {
        return Err("MAIL_OUTLOOK_COLLECTOR_UNAVAILABLE");
    }
    Ok(path)
}
#[cfg(windows)]
fn encode(value: &str) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut result = String::new();
    for group in value.as_bytes().chunks(3) {
        let a = group[0];
        let b = group.get(1).copied().unwrap_or(0);
        let c = group.get(2).copied().unwrap_or(0);
        let digits = [
            a >> 2,
            ((a & 3) << 4) | (b >> 4),
            ((b & 15) << 2) | (c >> 6),
            c & 63,
        ];
        for (i, v) in digits.into_iter().enumerate() {
            result.push(if i > group.len() {
                '='
            } else {
                DIGITS[v as usize] as char
            });
        }
    }
    result
}
#[cfg(windows)]
pub fn launch(context: &Context, locator: Option<(&str, &str)>) -> Result<()> {
    let path = executable(context)?;
    let root = context.state_root.join("mail");
    fs::create_dir_all(&root).map_err(|_| "MAIL_REGISTRY_UNAVAILABLE")?;
    let mut cmd = Command::new(path);
    cmd.env("LES_MAIL_STATE_ROOT", &root);
    les_core::prepare_child(&mut cmd);
    if let Some((store, entry)) = locator {
        if store.is_empty() || entry.is_empty() {
            return Err(BAD);
        }
        cmd.args(["--open", &encode(store), &encode(entry)]);
    } else {
        let id = les_core::chat_attachments::new_id()?;
        let temp = root.join(format!("collector-url-{id}.tmp"));
        let saved = root.join("collector_url.txt");
        let result = fs::write(
            &temp,
            format!(
                "http://127.0.0.1:{}/api/mail/collector/import",
                context.listen_port
            ),
        )
        .and_then(|_| fs::rename(&temp, &saved));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
            return Err("MAIL_REGISTRY_UNAVAILABLE");
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| "MAIL_OUTLOOK_COLLECTOR_UNAVAILABLE")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
#[cfg(not(windows))]
pub fn launch(_: &Context, _: Option<(&str, &str)>) -> Result<()> {
    Err("MAIL_OUTLOOK_COLLECTOR_UNAVAILABLE")
}
