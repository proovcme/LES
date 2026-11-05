use crate::config::{Failure, Result};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn stamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub struct Journal {
    state: PathBuf,
    pub run_id: String,
}
impl Journal {
    pub fn new(state: &Path, run_id: String) -> Self {
        Self {
            state: state.into(),
            run_id,
        }
    }
    pub fn event(&self, event: &str, fields: Value) -> Result<()> {
        let path = self.state.join("logs/native-runtime-lifecycle.jsonl");
        if fs::metadata(&path)
            .map(|m| m.len() >= 1_048_576)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(path.with_extension("jsonl.3"));
            for n in (1..=2).rev() {
                let _ = fs::rename(
                    path.with_extension(format!("jsonl.{n}")),
                    path.with_extension(format!("jsonl.{}", n + 1)),
                );
            }
            fs::rename(&path, path.with_extension("jsonl.1")).map_err(|_| {
                Failure::new("LOG_UNAVAILABLE", "Не удалось сохранить журнал запуска.")
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| Failure::new("LOG_UNAVAILABLE", "Не удалось открыть журнал запуска."))?;
        let record = json!({"schema":"les.lifecycle.v1","at":stamp(),"component":"native_launcher",
            "pid":std::process::id(),"run_id":self.run_id,"event":event,"details":fields});
        writeln!(file, "{record}")
            .and_then(|_| file.sync_data())
            .map_err(|_| Failure::new("LOG_UNAVAILABLE", "Не удалось записать причину остановки."))
    }
    pub fn status(&self, phase: &str, message: &str, fields: Value) -> Result<()> {
        let mut value = json!({"schema":"les.light-launcher.v1","phase":phase,"message":message,"updated_at":stamp(),
            "launcher_pid":std::process::id(),"run_id":self.run_id,"launcher_engine":"rust"});
        if let Some(fields) = fields.as_object() {
            value.as_object_mut().unwrap().extend(fields.clone());
        }
        let temporary = self
            .state
            .join(format!("launcher-status.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|_| {
                Failure::new(
                    "STATUS_UNAVAILABLE",
                    "Не удалось сохранить состояние запуска.",
                )
            })?;
        file.write_all(value.to_string().as_bytes())
            .and_then(|_| file.sync_data())
            .map_err(|_| {
                Failure::new(
                    "STATUS_UNAVAILABLE",
                    "Не удалось записать состояние запуска.",
                )
            })?;
        drop(file);
        fs::rename(temporary, self.state.join("launcher-status.json")).map_err(|_| {
            Failure::new(
                "STATUS_UNAVAILABLE",
                "Не удалось опубликовать состояние запуска.",
            )
        })
    }
}
