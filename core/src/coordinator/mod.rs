//! One Rho-owned runtime; host ports preserve LES context, effects and validation.
mod compatibility;
mod policy;
mod runtime;
pub use compatibility::run_research;
use policy::Policy;
use runtime::{Capability, Host, CONTROL};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
#[derive(Debug)]
pub enum Step {
    Model { final_answer: bool },
    Tool(Value),
    Complete(Value),
    Failed(String),
    Finished,
}
pub(super) enum Decision {
    Model(Value),
    Tool { payload: Value, projection: Value },
    Retry,
    Accept(Value),
    Abort(String),
}
pub(super) struct Envelope {
    step: Step,
    reply: Option<oneshot::Sender<Decision>>,
}
enum Phase {
    Idle,
    Model(bool),
    Tool(String),
    Complete,
    Finished,
}
pub struct Coordinator {
    policy: Arc<Mutex<Policy>>,
    events: mpsc::UnboundedReceiver<Envelope>,
    pending: Option<oneshot::Sender<Decision>>,
    phase: Phase,
    cancel: rho_sdk::CancellationToken,
    worker: Option<std::thread::JoinHandle<()>>,
    pub rounds: Vec<Value>,
    results: Vec<Value>,
}
impl Coordinator {
    pub fn new(
        maximum: u64,
        seconds: f64,
        allowed: impl IntoIterator<Item = String>,
    ) -> Result<Self, &'static str> {
        if maximum > 256 || !seconds.is_finite() || seconds <= 0. || seconds > 3600. {
            return Err("AGENT_REQUEST_INVALID");
        }
        let allowed: BTreeSet<_> = allowed.into_iter().collect();
        if allowed.len() > 128 || allowed.contains(CONTROL) {
            return Err("AGENT_REQUEST_INVALID");
        }
        let policy = Arc::new(Mutex::new(Policy {
            allowed: allowed.clone(),
            remaining: maximum,
            deadline: Instant::now() + Duration::from_secs_f64(seconds),
            closed: maximum == 0,
            reason: if maximum == 0 {
                "calls_budget"
            } else {
                "deadline"
            }
            .into(),
            failed: None,
            results: Vec::new(),
            rounds: Vec::new(),
            epochs: json!({}),
            binding: Value::Null,
            accepted: None,
            model_messages: Vec::new(),
            calls: HashMap::new(),
            data_result_ids: HashSet::new(),
            private_ids: HashSet::new(),
            completed: HashMap::new(),
            failed_calls: HashSet::new(),
            batch: 0,
            sequence: 0,
            workspace_epoch: 0,
            extension_epoch: 0,
            completion_revised: false,
            answer_window_prepared: false,
            empty_answers: 0,
        }));
        let (sender, events) = mpsc::unbounded_channel();
        let host = Host {
            policy: policy.clone(),
            events: sender.clone(),
        };
        let cancel = rho_sdk::CancellationToken::new();
        let token = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("les-rho-coordinator".into())
            .spawn(move || {
                let result = (|| -> Result<(), String> {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "RHO_RUNTIME_UNAVAILABLE")?;
                    rt.block_on(async move {
                        let mut builder = rho_sdk::Rho::builder()
                            .provider(host.clone())
                            .system_prompt(rho_sdk::SystemPrompt::None)
                            .max_steps(
                                std::num::NonZeroUsize::new(
                                    (maximum as usize).saturating_mul(2).saturating_add(12),
                                )
                                .unwrap(),
                            );
                        for name in allowed.into_iter().chain([CONTROL.into()]) {
                            builder = builder.tool(Capability {
                                host: host.clone(),
                                name,
                            });
                        }
                        let rho = builder.build().map_err(|_| "RHO_RUNTIME_UNAVAILABLE")?;
                        let session = rho
                            .session(rho_sdk::SessionOptions::default())
                            .await
                            .map_err(|_| "RHO_RUNTIME_UNAVAILABLE")?;
                        let mut run = session
                            .start(rho_sdk::UserInput::text("LES bound request"))
                            .await
                            .map_err(|_| "RHO_RUNTIME_UNAVAILABLE")?;
                        let handle = run.cancellation_handle();
                        let watcher = tokio::spawn(async move {
                            token.cancelled().await;
                            handle.cancel();
                        });
                        while run.next_event().await.is_some() {}
                        let outcome = run.outcome().await;
                        watcher.abort();
                        if let Some(code) = &host.policy.lock().unwrap().failed {
                            return Err(code.clone());
                        }
                        match outcome {
                            Ok(outcome)
                                if outcome.stop_reason() == rho_sdk::StopReason::EndTurn =>
                            {
                                Ok(())
                            }
                            Ok(_) => Err("AGENT_MODEL_STEP_LIMIT".into()),
                            Err(rho_sdk::Error::Provider(e)) => Err(e.message().to_owned()),
                            Err(_) => Err("RHO_RUN_FAILED".into()),
                        }
                    })
                })();
                let step = match result {
                    Ok(()) => Step::Finished,
                    Err(code) => Step::Failed(code),
                };
                let _ = sender.send(Envelope { step, reply: None });
            })
            .map_err(|_| "RHO_RUNTIME_UNAVAILABLE")?;
        Ok(Self {
            policy,
            events,
            pending: None,
            phase: Phase::Idle,
            cancel,
            worker: Some(worker),
            rounds: Vec::new(),
            results: Vec::new(),
        })
    }
    fn receive(&mut self, envelope: Option<Envelope>) -> Result<Step, &'static str> {
        let envelope = envelope.ok_or("RHO_RUNTIME_STOPPED")?;
        self.phase = match &envelope.step {
            Step::Model { final_answer } => Phase::Model(*final_answer),
            Step::Tool(call) => Phase::Tool(call["tool"].as_str().unwrap_or("").into()),
            Step::Complete(_) => Phase::Complete,
            _ => Phase::Finished,
        };
        self.pending = envelope.reply;
        let state = self.policy.lock().unwrap();
        self.rounds = state.rounds.clone();
        self.results = state.results.clone();
        Ok(envelope.step)
    }
    pub async fn next(&mut self) -> Result<Step, &'static str> {
        if self.pending.is_some() {
            return Err("AGENT_OPERATION_INFLIGHT");
        }
        let event = self.events.recv().await;
        self.receive(event)
    }
    pub fn next_blocking(&mut self) -> Result<Step, &'static str> {
        if self.pending.is_some() {
            return Err("AGENT_OPERATION_INFLIGHT");
        }
        let event = self.events.blocking_recv();
        self.receive(event)
    }
    fn reply(&mut self, decision: Decision) -> Result<(), &'static str> {
        self.pending
            .take()
            .ok_or("AGENT_OPERATION_NOT_REQUESTED")?
            .send(decision)
            .map_err(|_| "RHO_RUNTIME_STOPPED")?;
        self.phase = Phase::Idle;
        Ok(())
    }
    pub fn model_result(&mut self, response: Value) -> Result<(), &'static str> {
        self.model_result_bound(response, None)
    }
    pub fn model_result_bound(
        &mut self,
        response: Value,
        binding: Option<&Value>,
    ) -> Result<(), &'static str> {
        if !matches!(self.phase, Phase::Model(_)) {
            return Err("AGENT_MODEL_NOT_REQUESTED");
        }
        if let Some(binding) = binding {
            self.policy.lock().unwrap().binding = binding.clone();
        }
        self.reply(Decision::Model(response))
    }
    pub fn tool_result(&mut self, payload: Value) -> Result<(), &'static str> {
        self.tool_result_projected(payload.clone(), payload)
    }
    pub fn tool_result_projected(
        &mut self,
        payload: Value,
        projection: Value,
    ) -> Result<(), &'static str> {
        let Phase::Tool(expected) = &self.phase else {
            return Err("AGENT_TOOL_NOT_REQUESTED");
        };
        if payload["tool"] != *expected {
            self.fail("TOOL_RESULT_MISMATCH");
            return Err("TOOL_RESULT_MISMATCH");
        }
        {
            let mut policy = self.policy.lock().unwrap();
            // Commit host effects before acknowledging them. The caller may
            // immediately advance a table/review epoch after this returns.
            if let Err(code) = policy.record(payload.clone()) {
                policy.failed = Some(code);
            }
            self.results = policy.results.clone();
        }
        self.reply(Decision::Tool {
            payload,
            projection,
        })
    }
    pub fn retry_model(&mut self, reason: &str) -> Result<(), &'static str> {
        if !matches!(self.phase, Phase::Model(false)) || reason.len() > 100 {
            return Err("AGENT_MODEL_NOT_REQUESTED");
        }
        {
            let mut p = self.policy.lock().unwrap();
            let round = p.rounds.len() + 1;
            p.rounds.push(json!({"round":round,"proposed":0,"executed":0,"model_sent":true,"draft_rejected":reason}));
        }
        self.reply(Decision::Retry)
    }
    pub fn prepare_answer_window(&mut self) -> Result<(), &'static str> {
        {
            let mut p = self.policy.lock().unwrap();
            if p.answer_window_prepared
                || p.completion_revised
                || p.failed.is_some()
                || !matches!(self.phase, Phase::Model(false) | Phase::Complete)
            {
                return Err("AGENT_COMPLETION_REVISION_INVALID");
            }
            p.answer_window_prepared = true;
            p.closed = true;
        }
        self.reply(Decision::Retry)
    }
    pub fn revise_completion(&mut self) -> Result<(), &'static str> {
        {
            let mut p = self.policy.lock().unwrap();
            if p.completion_revised || p.failed.is_some() || !matches!(self.phase, Phase::Complete)
            {
                return Err("AGENT_COMPLETION_REVISION_INVALID");
            }
            p.completion_revised = true;
            p.closed = true;
        }
        self.reply(Decision::Retry)
    }
    pub fn fail(&mut self, code: impl Into<String>) {
        let code = code.into();
        self.policy.lock().unwrap().failed = Some(code.clone());
        if self.pending.is_some() {
            let _ = self.reply(Decision::Abort(code));
        }
    }
    pub fn remaining(&self) -> u64 {
        self.policy.lock().unwrap().remaining
    }
    pub fn stop_reason(&self) -> String {
        self.policy.lock().unwrap().reason.clone()
    }
    pub fn results(&self) -> &[Value] {
        &self.results
    }
    pub fn set_epochs(&mut self, value: Value) -> Result<(), &'static str> {
        self.policy.lock().unwrap().epochs(value)
    }
    pub fn update_epochs(&mut self, value: Value) -> Result<(), &'static str> {
        let mut p = self.policy.lock().unwrap();
        let mut v = p.epochs.clone();
        v.as_object_mut()
            .ok_or("AGENT_CONTEXT_INVALID")?
            .extend(value.as_object().ok_or("AGENT_CONTEXT_INVALID")?.clone());
        p.epochs(v)
    }
    pub fn capability_revision(&mut self, name: &str, revision: Value) -> Result<(), &'static str> {
        let mut p = self.policy.lock().unwrap();
        if !p.allowed.contains(name) || revision.to_string().len() > 1024 {
            return Err("AGENT_CONTEXT_INVALID");
        }
        if p.epochs[name] != revision {
            p.epochs[name] = revision;
            p.invalidate_extensions();
        }
        Ok(())
    }
    pub fn provider_messages(&self, native: bool) -> Vec<Value> {
        let messages = self.policy.lock().unwrap().model_messages.clone();
        if native {
            return messages;
        }
        messages
            .into_iter()
            .filter_map(|m| {
                let text = m["content"].as_str()?;
                serde_json::from_str(
                    text.strip_prefix("Результат продолжения чтения источника (данные): ")
                        .unwrap_or(text),
                )
                .ok()
            })
            .collect()
    }
    pub async fn accept_completed(&mut self, response: Value) -> Result<(), String> {
        self.reply(Decision::Accept(response))
            .map_err(str::to_owned)?;
        match self.next().await.map_err(str::to_owned)? {
            Step::Finished => Ok(()),
            Step::Failed(code) => Err(code),
            _ => Err("RHO_COMPLETION_NOT_FINAL".into()),
        }
    }
    pub fn accept_completed_blocking(&mut self, response: Value) -> Result<(), String> {
        self.reply(Decision::Accept(response))
            .map_err(str::to_owned)?;
        match self.next_blocking().map_err(str::to_owned)? {
            Step::Finished => Ok(()),
            Step::Failed(code) => Err(code),
            _ => Err("RHO_COMPLETION_NOT_FINAL".into()),
        }
    }
}
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.pending.take();
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests;
