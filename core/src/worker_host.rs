//! Bounded, reusable format worker. Rust owns timeouts, framing and process cleanup.
use crate::process_guard::{prepare, JobGuard};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, OnceLock, TryLockError,
    },
    thread,
    time::{Duration, Instant},
};

const REQUEST_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 32 * 1024 * 1024 - 65536;
static POOL: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static PDF_POOL: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn frame(stream: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>, &'static str> {
    let mut result = Vec::new();
    loop {
        let block = stream.fill_buf().map_err(|_| "WORKER_OUTPUT_FAILED")?;
        if block.is_empty() {
            return if result.is_empty() {
                Ok(None)
            } else {
                Err("WORKER_RESPONSE_INCOMPLETE")
            };
        }
        let end = block.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let count = end.unwrap_or(block.len());
        if result.len() + count > limit {
            return Err("WORKER_OUTPUT_LIMIT");
        }
        result.extend_from_slice(&block[..count]);
        stream.consume(count);
        if end.is_some() {
            return Ok(Some(result));
        }
    }
}

struct Session {
    child: Child,
    guard: Option<JobGuard>,
    input: Option<ChildStdin>,
    output: Option<mpsc::Receiver<Result<Vec<u8>, &'static str>>>,
    errors: Arc<Mutex<VecDeque<u8>>>,
    readers: Vec<thread::JoinHandle<()>>,
    key: String,
    last_used: Instant,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl Session {
    fn terminate(&mut self) {
        self.output.take(); // Unblock a sender even when a broken worker emitted extra frames.
        drop(self.guard.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
    fn spawn(
        program: &str,
        args: &[&str],
        directory: &str,
        key: String,
    ) -> Result<Self, &'static str> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepare(&mut command);
        let mut child = command.spawn().map_err(|_| "WORKER_START_FAILED")?;
        let guard = match JobGuard::own(&child, Some(2 * 1024 * 1024 * 1024)) {
            Ok(value) => value,
            Err(code) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(code);
            }
        };
        let output = child.stdout.take().ok_or("WORKER_PIPE_FAILED")?;
        let mut stderr = child.stderr.take().ok_or("WORKER_PIPE_FAILED")?;
        let input = child.stdin.take();
        let (sender, receiver) = mpsc::sync_channel(1);
        let out_reader = thread::spawn(move || {
            let mut stream = BufReader::new(output);
            loop {
                match frame(&mut stream, OUTPUT_LIMIT) {
                    Ok(Some(bytes)) => {
                        if sender.send(Ok(bytes)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Err("WORKER_EXITED"));
                        break;
                    }
                    Err(code) => {
                        let _ = sender.send(Err(code));
                        break;
                    }
                }
            }
        });
        let errors = Arc::new(Mutex::new(VecDeque::new()));
        let collected = errors.clone();
        let err_reader = thread::spawn(move || {
            let mut block = [0u8; 8192];
            while let Ok(count) = stderr.read(&mut block) {
                if count == 0 {
                    break;
                }
                if let Ok(mut bytes) = collected.lock() {
                    bytes.extend(&block[..count]);
                    let excess = bytes.len().saturating_sub(65536);
                    bytes.drain(..excess);
                }
            }
        });
        Ok(Self {
            child,
            guard: Some(guard),
            input,
            output: Some(receiver),
            errors,
            readers: vec![out_reader, err_reader],
            key,
            last_used: Instant::now(),
        })
    }
    fn invoke(
        &mut self,
        input: &Value,
        deadline: Instant,
        generation: u64,
    ) -> Result<Value, &'static str> {
        if let Ok(mut errors) = self.errors.lock() {
            errors.clear();
        }
        let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut data = serde_json::to_vec(&json!({"request_id":id,"input":input}))
            .map_err(|_| "WORKER_INPUT_INVALID")?;
        if data.len() > REQUEST_LIMIT {
            return Err("WORKER_INPUT_LIMIT");
        }
        data.push(b'\n');
        let mut pipe = self.input.take().ok_or("WORKER_PIPE_FAILED")?;
        let writer = thread::spawn(move || {
            let sent = pipe.write_all(&data).and_then(|_| pipe.flush());
            (pipe, sent)
        });
        let mut cancelled = false;
        let output = self.output.as_ref().ok_or("WORKER_PIPE_FAILED")?;
        let mut result = loop {
            if GENERATION.load(Ordering::SeqCst) != generation {
                cancelled = true;
                break Err(mpsc::RecvTimeoutError::Disconnected);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match output.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                result => break result,
            }
        };
        while !writer.is_finished()
            && Instant::now() < deadline
            && result.is_ok()
            && GENERATION.load(Ordering::SeqCst) == generation
        {
            thread::sleep(Duration::from_millis(10));
        }
        if GENERATION.load(Ordering::SeqCst) != generation {
            cancelled = true;
            result = Err(mpsc::RecvTimeoutError::Disconnected);
        }
        if !writer.is_finished() {
            result = Err(mpsc::RecvTimeoutError::Timeout);
        }
        if result.is_err() || matches!(result, Ok(Err(_))) {
            drop(self.guard.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let (pipe, sent) = writer.join().map_err(|_| "WORKER_INPUT_FAILED")?;
        self.input = Some(pipe);
        if cancelled {
            return Err("WORKER_CANCELLED");
        }
        let bytes = match result {
            Ok(value) => value?,
            Err(mpsc::RecvTimeoutError::Timeout) => return Err("WORKER_TIMEOUT"),
            Err(_) => return Err("WORKER_EXITED"),
        };
        if sent.is_err() {
            return Err("WORKER_INPUT_FAILED");
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "WORKER_RESPONSE_INVALID")?;
        if value["request_id"] != id {
            return Err("WORKER_RESPONSE_INVALID");
        }
        Ok(value["result"].clone())
    }
    fn diagnostics(&self) -> String {
        self.errors
            .lock()
            .map(|bytes| {
                String::from_utf8_lossy(&bytes.iter().copied().collect::<Vec<_>>()).into_owned()
            })
            .unwrap_or_default()
    }
}

pub fn run(request: &Value) -> Result<Value, &'static str> {
    run_in(request, &POOL)
}

pub fn run_pdf(request: &Value) -> Result<Value, &'static str> {
    run_in(request, &PDF_POOL)
}

fn run_in(
    request: &Value,
    pool: &'static OnceLock<Mutex<Option<Session>>>,
) -> Result<Value, &'static str> {
    if request["schema"] != "les.worker-host.v1" {
        return Err("WORKER_HOST_SCHEMA_INVALID");
    }
    let program = request["program"]
        .as_str()
        .ok_or("WORKER_PROGRAM_INVALID")?;
    let directory = request["directory"]
        .as_str()
        .ok_or("WORKER_DIRECTORY_INVALID")?;
    if !Path::new(program).is_absolute()
        || !Path::new(program).is_file()
        || !Path::new(directory).is_absolute()
        || !Path::new(directory).is_dir()
    {
        return Err("WORKER_PROGRAM_INVALID");
    }
    let args = request["args"].as_array().ok_or("WORKER_ARGS_INVALID")?;
    if args.len() > 32 {
        return Err("WORKER_ARGS_INVALID");
    }
    let args: Vec<&str> = args
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= 16384)
                .ok_or("WORKER_ARGS_INVALID")
        })
        .collect::<Result<_, _>>()?;
    let timeout = request["timeout_ms"]
        .as_u64()
        .filter(|n| (100..=3_600_000).contains(n))
        .ok_or("WORKER_TIMEOUT_INVALID")?;
    let key = serde_json::to_string(&json!([program, args, directory, request["context"]]))
        .map_err(|_| "WORKER_INPUT_INVALID")?;
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let generation = GENERATION.load(Ordering::SeqCst);
    let pool = pool.get_or_init(|| {
        // One idle reaper per host, not a timer/thread per document. Never cancel
        // another dataset's active conversion merely because one batch finished.
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            if let Some(pool) = pool.get() {
                if let Ok(mut slot) = pool.try_lock() {
                    if slot.as_ref().is_some_and(|session| {
                        session.last_used.elapsed() >= Duration::from_secs(60)
                    }) {
                        slot.take();
                    }
                }
            }
        });
        Mutex::new(None)
    });
    let mut slot = loop {
        if GENERATION.load(Ordering::SeqCst) != generation {
            return Err("WORKER_CANCELLED");
        }
        match pool.try_lock() {
            Ok(value) => break value,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Poisoned(value)) => {
                let mut slot = value.into_inner();
                slot.take();
                pool.clear_poison();
                return Err("WORKER_HOST_RECOVERED");
            }
        }
        if Instant::now() >= deadline {
            return Err("WORKER_QUEUE_TIMEOUT");
        }
        thread::sleep(Duration::from_millis(10));
    };
    if GENERATION.load(Ordering::SeqCst) != generation {
        return Err("WORKER_CANCELLED");
    }
    if slot.as_ref().is_some_and(|session| session.key != key) {
        slot.take();
    }
    if slot
        .as_mut()
        .is_some_and(|session| session.child.try_wait().ok().flatten().is_some())
    {
        slot.take();
    }
    let reused = slot.is_some();
    if slot.is_none() {
        *slot = Some(Session::spawn(program, &args, directory, key)?);
    }
    let session = slot.as_mut().unwrap();
    let pid = session.child.id();
    match session.invoke(&request["input"], deadline, generation) {
        Ok(result) => {
            session.last_used = Instant::now();
            let diagnostics = if result["status"] == "failed" {
                session.diagnostics()
            } else {
                String::new()
            };
            Ok(
                json!({"status":"completed","result":result,"worker_pid":pid,"reused":reused,"diagnostics":diagnostics}),
            )
        }
        Err(code) => {
            session.terminate();
            let status = session.child.try_wait().ok().flatten();
            let diagnostics = session.diagnostics();
            slot.take();
            Ok(
                json!({"status":"failed","code":code,"exit_code":status.and_then(|code|code.code()),"worker_pid":pid,"diagnostics":diagnostics}),
            )
        }
    }
}

pub fn release_idle() -> Result<Value, &'static str> {
    let mut released = false;
    let mut busy = false;
    for pool in [POOL.get(), PDF_POOL.get()].into_iter().flatten() {
        match pool.try_lock() {
            Ok(mut slot) => released |= slot.take().is_some(),
            Err(TryLockError::WouldBlock) => busy = true,
            Err(_) => return Err("WORKER_HOST_BUSY"),
        }
    }
    Ok(if busy {
        json!({"released":released,"busy":true})
    } else {
        json!({"released":released})
    })
}

pub fn shutdown() -> Result<Value, &'static str> {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    for pool in [POOL.get(), PDF_POOL.get()].into_iter().flatten() {
        pool.lock().map_err(|_| "WORKER_HOST_BUSY")?.take();
    }
    Ok(json!({"stopped":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_requests_never_launch_a_worker() {
        assert_eq!(run(&json!({})).unwrap_err(), "WORKER_HOST_SCHEMA_INVALID");
        assert_eq!(
            run(
                &json!({"schema":"les.worker-host.v1","program":"relative","directory":"relative"})
            )
            .unwrap_err(),
            "WORKER_PROGRAM_INVALID"
        );
    }
    #[test]
    fn framing_is_bounded_and_rejects_partial_responses() {
        assert_eq!(
            frame(&mut &b"abc\n"[..], 3).unwrap_err(),
            "WORKER_OUTPUT_LIMIT"
        );
        assert_eq!(
            frame(&mut &b"abc"[..], 100).unwrap_err(),
            "WORKER_RESPONSE_INCOMPLETE"
        );
        assert_eq!(frame(&mut &b"abc\n"[..], 100).unwrap().unwrap(), b"abc\n");
    }
}
