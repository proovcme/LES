//! Lifecycle records contain ownership and stable reasons, never document text.
use std::{fs, io::Write, path::Path, sync::Mutex, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use serde_json::{json, Value};

static WRITER: Mutex<()> = Mutex::new(());

pub fn stamp() -> f64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64() }

pub fn event(state: &Path, component: &str, name: &str, fields: Value) {
    let Ok(_guard) = WRITER.lock() else { return; };
    let folder = state.join("logs");
    if fs::create_dir_all(&folder).is_err() { return; }
    let path = folder.join(format!("{component}-lifecycle.jsonl"));
    if fs::metadata(&path).map(|m| m.len() >= 1_048_576).unwrap_or(false) {
        let previous = path.with_extension("jsonl.1");
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&path, previous);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let record = json!({"schema":"les.lifecycle.v1", "at":stamp(), "component":component,
                           "pid":std::process::id(), "event":name, "details":fields});
        let _ = writeln!(file, "{record}");
        let _ = file.sync_data();
    }
}

pub fn status(state: &Path, file: &str, value: Value) {
    let temporary = state.join(format!("{file}.{}.tmp", std::process::id()));
    if fs::write(&temporary, value.to_string()).is_ok() { let _ = fs::rename(temporary, state.join(file)); }
}

#[derive(Default)]
pub struct Recovery { attempts: Vec<Instant> }

impl Recovery {
    pub fn next(&mut self, now: Instant) -> Option<Duration> {
        self.attempts.retain(|then| now.duration_since(*then) < Duration::from_secs(300));
        if self.attempts.len() >= 3 { return None; }
        let delay = Duration::from_secs(1 << self.attempts.len());
        self.attempts.push(now);
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_budget_is_bounded_and_old_failures_expire() {
        let start = Instant::now(); let mut policy = Recovery::default();
        assert_eq!(policy.next(start), Some(Duration::from_secs(1)));
        assert_eq!(policy.next(start), Some(Duration::from_secs(2)));
        assert_eq!(policy.next(start), Some(Duration::from_secs(4)));
        assert_eq!(policy.next(start), None);
        assert_eq!(policy.next(start + Duration::from_secs(301)), Some(Duration::from_secs(1)));
    }
}
