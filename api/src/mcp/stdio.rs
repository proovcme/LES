//! Owned MCP stdio process. No shell or inherited application/model credentials.
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Instant,
};
pub struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<Receiver<Result<Value, &'static str>>>,
    guard: Option<les_core::JobGuard>,
    readers: Vec<JoinHandle<()>>,
}
impl Drop for Process {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        self.guard.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.readers.drain(..) {
            let _ = thread.join();
        }
    }
}
impl Process {
    pub fn start(connection: &Value) -> Result<Self, &'static str> {
        let program = connection["command"]
            .as_str()
            .filter(|v| Path::new(v).is_absolute() && Path::new(v).is_file())
            .ok_or("MCP_PROGRAM_MISSING")?;
        let args = connection["args"]
            .as_array()
            .ok_or("MCP_CONFIG_INVALID")?
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| s.len() <= 4096 && !s.contains('\0'))
                    .ok_or("MCP_CONFIG_INVALID")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if args.len() > 32 {
            return Err("MCP_CONFIG_INVALID");
        }
        let mut command = Command::new(program);
        if let Some(directory) = connection["cwd"].as_str() {
            let path = Path::new(directory);
            if !path.is_absolute() || !path.is_dir() {
                return Err("MCP_DIRECTORY_INVALID");
            }
            command.current_dir(path);
        }
        command
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in std::env::vars_os() {
            if matches!(
                name.to_string_lossy().to_uppercase().as_str(),
                "HOME"
                    | "LOGNAME"
                    | "PATH"
                    | "SHELL"
                    | "TERM"
                    | "USER"
                    | "APPDATA"
                    | "HOMEDRIVE"
                    | "HOMEPATH"
                    | "LOCALAPPDATA"
                    | "PATHEXT"
                    | "PROCESSOR_ARCHITECTURE"
                    | "SYSTEMDRIVE"
                    | "SYSTEMROOT"
                    | "TEMP"
                    | "TMP"
                    | "USERDOMAIN"
                    | "USERNAME"
                    | "USERPROFILE"
                    | "WINDIR"
                    | "LANG"
            ) {
                command.env(name, value);
            }
        }
        les_core::prepare_child(&mut command);
        let child = command.spawn().map_err(|_| "MCP_START_FAILED")?;
        let mut process = Self {
            child,
            input: None,
            output: None,
            guard: None,
            readers: Vec::new(),
        };
        process.guard = Some(les_core::JobGuard::own(
            &process.child,
            Some(2 * 1024 * 1024 * 1024),
        )?);
        process.input = process.child.stdin.take();
        let output = process.child.stdout.take().ok_or("MCP_PIPE_FAILED")?;
        let mut errors = process.child.stderr.take().ok_or("MCP_PIPE_FAILED")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        process.output = Some(receiver);
        process.readers.push(thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut frame = Vec::new();
                let read = (&mut reader).take(1_000_002).read_until(b'\n', &mut frame);
                let value = match read {
                    Ok(0) => Err("MCP_PROCESS_STOPPED"),
                    Ok(_) if frame.len() > 1_000_001 || !frame.ends_with(b"\n") => {
                        Err("MCP_RESPONSE_LIMIT")
                    }
                    Ok(_) => serde_json::from_slice(&frame).map_err(|_| "MCP_RESPONSE_INVALID"),
                    Err(_) => Err("MCP_PIPE_FAILED"),
                };
                let stop = value.is_err();
                if sender.send(value).is_err() || stop {
                    break;
                }
            }
        }));
        process.readers.push(thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            loop {
                match errors.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }));
        Ok(process)
    }
    pub fn send(&mut self, value: &Value) -> Result<(), &'static str> {
        let bytes = serde_json::to_vec(value).map_err(|_| "MCP_REQUEST_INVALID")?;
        if bytes.len() > 256000 {
            return Err("MCP_REQUEST_LIMIT");
        }
        let input = self.input.as_mut().ok_or("MCP_PIPE_FAILED")?;
        input
            .write_all(&bytes)
            .and_then(|_| input.write_all(b"\n"))
            .and_then(|_| input.flush())
            .map_err(|_| "MCP_PIPE_FAILED")
    }
    pub fn receive(&mut self, deadline: Instant) -> Result<Value, &'static str> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("MCP_TIMEOUT");
        }
        self.output
            .as_ref()
            .ok_or("MCP_PIPE_FAILED")?
            .recv_timeout(left)
            .map_err(|_| "MCP_TIMEOUT")?
    }
}
