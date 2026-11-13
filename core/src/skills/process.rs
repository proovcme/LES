//! One short-lived compatibility interpreter; no application modules or environment in the child.
use super::{absolute, atomic, json_file, storage, worker, Result};
use crate::process_guard::{prepare, JobGuard};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct Host {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<Receiver<std::result::Result<Value, &'static str>>>,
    guard: Option<JobGuard>,
    readers: Vec<JoinHandle<()>>,
    module_roots: Vec<std::path::PathBuf>,
    dll_roots: Vec<std::path::PathBuf>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        self.guard.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}
impl Host {
    fn start(request: &Value, work: &Path) -> std::result::Result<Self, &'static str> {
        let program = absolute(request, "program").map_err(|_| "SKILL_RUNTIME_MISSING")?;
        if !program.is_file() {
            return Err("SKILL_RUNTIME_MISSING");
        }
        // Preserve the operator's existing venv dependencies without executing
        // site/.pth startup or adding the application's source directory.
        let (mut module_roots, dll_roots) =
            super::runtime::dependency_paths(&program).map_err(|_| "SKILL_RUNTIME_INVALID")?;
        if let Some(root) = program
            .parent()
            .and_then(Path::parent)
            .filter(|root| root.join("pyvenv.cfg").is_file())
        {
            let modules = root.join("Lib/site-packages");
            if modules.is_dir() {
                module_roots.push(modules);
            }
            let lib = root.join("lib");
            if let Ok(entries) = std::fs::read_dir(lib) {
                for entry in entries.take(32).flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("python")
                        && name.len() <= 16
                        && name[6..].chars().all(|c| c.is_ascii_digit() || c == '.')
                    {
                        let modules = entry.path().join("site-packages");
                        if modules.is_dir() {
                            module_roots.push(modules);
                        }
                    }
                }
            }
        }
        // Windows venv python.exe is a redirector that would need a second process.
        // Resolve its own pyvenv.cfg to the exact base interpreter before applying the one-process guard.
        #[cfg(windows)]
        if let Some(venv) = program.parent().and_then(Path::parent) {
            let config = venv.join("pyvenv.cfg");
            if config.is_file() {
                let mut bytes = Vec::new();
                std::fs::File::open(&config)
                    .map_err(|_| "SKILL_RUNTIME_MISSING")?
                    .take(65537)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "SKILL_RUNTIME_MISSING")?;
                if bytes.len() > 65536 {
                    return Err("SKILL_RUNTIME_MISSING");
                }
                let config = String::from_utf8(bytes).map_err(|_| "SKILL_RUNTIME_MISSING")?;
                let fields = config
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .map(|(k, v)| (k.trim(), v.trim()))
                    .collect::<std::collections::HashMap<_, _>>();
                let base = fields
                    .get("executable")
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        fields
                            .get("home")
                            .map(|home| Path::new(home).join("python.exe"))
                    })
                    .ok_or("SKILL_RUNTIME_MISSING")?;
                if !base.is_absolute() || !base.is_file() {
                    return Err("SKILL_RUNTIME_MISSING");
                }
                program = base;
            }
        }
        let mut command = Command::new(program);
        command
            .args(["-I", "-S", "-B", "-X", "utf8", "-c", worker::SCRIPT])
            .current_dir(work)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in std::env::vars_os() {
            if matches!(
                name.to_string_lossy().to_uppercase().as_str(),
                "SYSTEMROOT" | "WINDIR" | "TEMP" | "TMP"
            ) {
                command.env(name, value);
            }
        }
        for name in [
            "OPENBLAS_NUM_THREADS",
            "OMP_NUM_THREADS",
            "MKL_NUM_THREADS",
            "NUMEXPR_NUM_THREADS",
        ] {
            command.env(name, "1");
        }
        prepare(&mut command);
        let child = command.spawn().map_err(|_| "SKILL_RUNTIME_START_FAILED")?;
        let mut host = Host {
            child,
            input: None,
            output: None,
            guard: None,
            readers: Vec::new(),
            module_roots,
            dll_roots,
        };
        host.guard = Some(JobGuard::skill(&host.child)?);
        host.input = host.child.stdin.take();
        let stdout = host.child.stdout.take().ok_or("SKILL_PIPE_FAILED")?;
        let mut stderr = host.child.stderr.take().ok_or("SKILL_PIPE_FAILED")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        host.output = Some(receiver);
        host.readers.push(thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let frame = (&mut reader).take(128002).read_until(b'\n', &mut line);
                let result = match frame {
                    Ok(0) => Err("SKILL_PROCESS_FAILED"),
                    Ok(_) if line.len() > 128001 || !line.ends_with(b"\n") => {
                        Err("SKILL_RESULT_LIMIT")
                    }
                    Ok(_) => serde_json::from_slice(&line).map_err(|_| "SKILL_PROTOCOL_INVALID"),
                    Err(_) => Err("SKILL_PIPE_FAILED"),
                };
                let stop = result.is_err();
                if sender.send(result).is_err() || stop {
                    break;
                }
            }
        }));
        host.readers.push(thread::spawn(move || {
            let mut block = [0u8; 8192];
            loop {
                match stderr.read(&mut block) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }));
        Ok(host)
    }
    fn send(&mut self, value: &Value) -> std::result::Result<(), &'static str> {
        let input = self.input.as_mut().ok_or("SKILL_PIPE_FAILED")?;
        let encoded = serde_json::to_vec(value).map_err(|_| "SKILL_PROTOCOL_INVALID")?;
        input
            .write_all(&encoded)
            .and_then(|_| input.write_all(b"\n"))
            .and_then(|_| input.flush())
            .map_err(|_| "SKILL_PIPE_FAILED")
    }
    fn receive(&self, deadline: Instant) -> std::result::Result<Value, &'static str> {
        let receiver = self.output.as_ref().ok_or("SKILL_PIPE_FAILED")?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("SKILL_DEADLINE_EXCEEDED");
            }
            match receiver.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(value) => return value,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("SKILL_PROCESS_FAILED"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            #[cfg(target_os = "macos")]
            {
                let output = Command::new("/bin/ps")
                    .args(["-o", "rss=", "-p", &self.child.id().to_string()])
                    .output()
                    .map_err(|_| "SKILL_MEMORY_CHECK_FAILED")?;
                if let Ok(rss) = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<u64>()
                {
                    if rss > 512 * 1024 {
                        return Err("SKILL_MEMORY_LIMIT");
                    }
                }
            }
        }
    }
}

pub(super) fn run(
    request: &Value,
    work: &Path,
    input: &Value,
    rows: &mut Vec<Value>,
) -> Result<Option<String>> {
    let deadline = Instant::now()
        + Duration::from_millis(
            request["timeout_ms"]
                .as_u64()
                .unwrap_or(20000)
                .clamp(100, 20000),
        );
    let mut host = match Host::start(request, work) {
        Ok(host) => host,
        Err(code) => {
            json_file(
                &work.join("failure.json"),
                &json!({"code":code,"stage":"start"}),
            )?;
            return Ok(Some(code.into()));
        }
    };
    json_file(
        &work.join("execution.json"),
        &json!({"worker_pid":host.child.id(),"owner_pid":std::process::id(),"runtime":"cpython.skill-only"}),
    )?;
    // Lifetime guard is attached before the first protocol frame releases user code.
    let result = (|| -> std::result::Result<(), &'static str> {
        host.send(
            &json!({"sha256":input["sha256"],"entrypoint":input["entrypoint"],
            "module_roots":host.module_roots,"dll_roots":host.dll_roots}),
        )?;
        let initial = host.receive(deadline)?;
        if initial["fatal"].is_string() {
            let _ = json_file(&work.join("failure.json"), &initial);
            return Err(if initial["fatal"] == "SKILL_DEPENDENCY_MISSING" {
                "SKILL_DEPENDENCY_MISSING"
            } else {
                "SKILL_SCRIPT_FAILED"
            });
        }
        if initial["ready"] != true {
            return Err("SKILL_PROTOCOL_INVALID");
        }
        for item in input["items"]
            .as_array()
            .ok_or("SKILL_PROTOCOL_INVALID")?
            .iter()
            .skip(rows.len())
        {
            host.send(item)?;
            let row = host.receive(deadline)?;
            if row["fatal"].is_string() {
                let _ = json_file(&work.join("failure.json"), &row);
                return Err(if row["fatal"] == "SKILL_DEPENDENCY_MISSING" {
                    "SKILL_DEPENDENCY_MISSING"
                } else {
                    "SKILL_SCRIPT_FAILED"
                });
            }
            if row["id"] != item["id"] || !matches!(row["status"].as_str(), Some("ok" | "error")) {
                return Err("SKILL_PROTOCOL_INVALID");
            }
            let encoded = serde_json::to_vec(&row).map_err(|_| "SKILL_RESULT_INVALID")?;
            if encoded.len() > 128000 {
                return Err("SKILL_RESULT_LIMIT");
            }
            atomic(&work.join(format!("{:04}.json", rows.len())), &encoded)
                .map_err(|_| "SKILL_STORAGE_FAILED")?;
            rows.push(row);
        }
        Ok(())
    })();
    if let Err(code) = result {
        let exit = host
            .child
            .try_wait()
            .map_err(|_| storage())?
            .and_then(|v| v.code());
        json_file(
            &work.join("stop.json"),
            &json!({"code":code,"exit_code":exit,"completed":rows.len()}),
        )?;
        return Ok(Some(code.into()));
    }
    let _ = std::fs::remove_file(work.join("stop.json"));
    let _ = std::fs::remove_file(work.join("failure.json"));
    Ok(None)
}

/// Native administrative CLI: one bounded JSON request on stdin, no server required.
pub fn serve_once() -> std::result::Result<(), &'static str> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "SKILL_REQUEST_INVALID")?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("SKILL_REQUEST_LIMIT");
    }
    let request = serde_json::from_slice(&bytes).map_err(|_| "SKILL_REQUEST_INVALID")?;
    println!("{}", super::execute(&request));
    Ok(())
}
