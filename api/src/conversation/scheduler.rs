//! One durable idle worker per chat owner. Foreground activity cancels its model body.
use super::{job, Result};
use crate::Context;
use futures_util::future::{select, Either};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{watch, Notify, OwnedSemaphorePermit};

pub struct Scheduler {
    started: AtomicBool,
    wake: Notify,
    epoch: watch::Sender<u64>,
    foreground: Mutex<HashMap<String, Lease>>,
}
struct Lease {
    deadline: Instant,
    _permit: Option<OwnedSemaphorePermit>,
}
impl Scheduler {
    pub fn new() -> Self {
        let (epoch, _) = watch::channel(0);
        Self {
            started: AtomicBool::new(false),
            wake: Notify::new(),
            epoch,
            foreground: Mutex::new(HashMap::new()),
        }
    }
    fn busy(&self) -> bool {
        let mut leases = self.foreground.lock().unwrap_or_else(|e| e.into_inner());
        leases.retain(|_, v| v.deadline > Instant::now());
        !leases.is_empty()
    }
    fn enter(&self) -> String {
        // The key is process-local; it never authorizes a request or survives recovery.
        let mut leases = self.foreground.lock().unwrap_or_else(|e| e.into_inner());
        let mut number = 0;
        self.epoch.send_modify(|epoch| {
            *epoch = epoch.wrapping_add(1);
            number = *epoch;
        });
        let key = format!("foreground-{number}");
        leases.insert(
            key.clone(),
            Lease {
                deadline: Instant::now() + Duration::from_secs(660),
                _permit: None,
            },
        );
        self.wake.notify_one();
        key
    }
    fn leave(&self, key: &str) {
        self.foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
        self.wake.notify_one();
    }
}
pub struct Foreground {
    context: Arc<Context>,
    key: String,
}
impl Foreground {
    pub fn new(context: Arc<Context>) -> Self {
        let key = context
            .chat
            .get()
            .expect("initialized chat owner")
            .summary
            .enter();
        Self { context, key }
    }
    /// The legacy caller uses the saved role's local/remote pool, without inference.
    pub async fn reserve(mut self) -> Result<String> {
        super::catalog(&self.context, "model_registry_init", json!({})).await?;
        let binding = super::catalog(
            &self.context,
            "model_execution_snapshot",
            json!({"role":"answer"}),
        )
        .await?;
        let service = self
            .context
            .chat
            .get()
            .ok_or("SUMMARY_SERVICE_UNAVAILABLE")?;
        let permit = tokio::time::timeout(
            Duration::from_secs(30),
            if binding["revision"]["locality"] == "remote" {
                service.remote.clone()
            } else {
                service.local.clone()
            }
            .acquire_owned(),
        )
        .await
        .map_err(|_| "MODEL_QUEUE_TIMEOUT")?
        .map_err(|_| "MODEL_QUEUE_TIMEOUT")?;
        service
            .summary
            .foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&self.key)
            .ok_or("SUMMARY_CLAIM_CHANGED")?
            ._permit = Some(permit);
        // Ownership transfers to the matching authenticated foreground_end call.
        let key = std::mem::take(&mut self.key);
        Ok(key)
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        if self.key.is_empty() {
            return;
        }
        self.context.chat.get().unwrap().summary.leave(&self.key);
    }
}
pub async fn end(context: &Arc<Context>, key: &str, sid: Option<&str>) -> Result<()> {
    context
        .chat
        .get()
        .ok_or("SUMMARY_SERVICE_UNAVAILABLE")?
        .summary
        .leave(key);
    if let Some(sid) = sid {
        enqueue(context, sid).await?;
    }
    Ok(())
}
pub async fn enqueue(context: &Arc<Context>, sid: &str) -> Result<()> {
    let db = context.database.clone();
    let session = sid.to_owned();
    let registered = tokio::task::spawn_blocking(move || crate::chat::unwrap(les_core::workspace::execute(
        &json!({"schema":"les.workspace.v1","database":db,"action":"get","arguments":{"session_id":session}}))))
        .await.map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?.map_err(|e|e.code)?;
    if registered["registered"] != true {
        return Ok(());
    }
    job(context, "enqueue", json!({"session_id":sid})).await?;
    context
        .chat
        .get()
        .ok_or("SUMMARY_SERVICE_UNAVAILABLE")?
        .summary
        .wake
        .notify_one();
    Ok(())
}
pub fn start(context: &Arc<Context>) {
    let service = context.chat.get().expect("initialized chat owner");
    if service.summary.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let context = context.clone();
    tokio::spawn(async move {
        run(context).await;
    });
}
pub async fn resume(context: Arc<Context>) {
    if std::env::var("LES_STARTUP_BACKGROUND_MUTATIONS").as_deref() != Ok("true") {
        return;
    }
    let db = context.database.clone();
    let pending =
        tokio::task::spawn_blocking(move || les_core::conversation_summary::queue::pending(&db))
            .await;
    match pending {
        Ok(Ok(true)) => {
            if let Err(e) = crate::chat::http::initialize(&context).await {
                eprintln!(
                    "{}",
                    json!({"event":"conversation_summary_recovery_failed","code":e.code})
                );
            }
        }
        Ok(Ok(false)) => {}
        _ => eprintln!(
            "{}",
            json!({"event":"conversation_summary_recovery_failed","code":"SUMMARY_STORAGE_UNAVAILABLE"})
        ),
    }
}
async fn finish(context: &Context, claim: &Value, code: &str, retry: bool) {
    let _ = job(context,"finish",json!({"session_id":claim["session_id"],"claim_token":claim["claim_token"],"code":code,"retry":retry})).await;
}
async fn run(context: Arc<Context>) {
    loop {
        let scheduler = &context.chat.get().unwrap().summary;
        // Debounce every wake; no model loads while the user is actively chatting.
        if tokio::time::timeout(Duration::from_secs(2), scheduler.wake.notified())
            .await
            .is_ok()
        {
            continue;
        }
        if scheduler.busy() {
            continue;
        }
        let mut epoch = scheduler.epoch.subscribe();
        let claim = match job(&context, "claim", json!({})).await {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                scheduler.wake.notified().await;
                continue;
            }
            Err(code) => {
                eprintln!(
                    "{}",
                    json!({"event":"conversation_summary_queue_failed","code":code})
                );
                continue;
            }
        };
        if scheduler.busy() || epoch.has_changed().unwrap_or(true) {
            finish(&context, &claim, "SUMMARY_PREEMPTED", true).await;
            continue;
        }
        let sid = claim["session_id"].as_str().unwrap().to_owned();
        let task = Box::pin(super::execute(
            context.clone(),
            &sid,
            false,
            None,
            Some(&claim),
        ));
        let result = match select(task, Box::pin(epoch.changed())).await {
            Either::Left((value, _)) => value,
            Either::Right(_) => {
                finish(&context, &claim, "SUMMARY_PREEMPTED", true).await;
                continue;
            }
        };
        match result {
            Ok(_) => {
                // A no-op has not committed its receipt. A real batch commits
                // its continuation flag together with the memory revision.
                let status = job(&context, "status", json!({"session_id":sid}))
                    .await
                    .unwrap_or(Value::Null);
                if status["status"] == "running" {
                    finish(&context, &claim, "", false).await;
                }
            }
            Err(code) => {
                let safe = code
                    .split(':')
                    .next()
                    .filter(|s| {
                        s.len() <= 100
                            && s.bytes()
                                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    })
                    .unwrap_or("SUMMARY_UPDATE_FAILED");
                finish(&context, &claim, safe, code == "SUMMARY_CONTEXT_CHANGED").await;
                eprintln!(
                    "{}",
                    json!({"event":"conversation_summary_failed","code":safe})
                );
            }
        }
    }
}
