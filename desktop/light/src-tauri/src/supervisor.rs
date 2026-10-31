//! Windows outer owner. A killed desktop cannot kill its independent observer.
//! The desktop waits for assignment to an owned job before starting any services.
use std::{fs, io::{Read, Write}, path::Path, process::{Command, Stdio}, thread, time::Instant};
use serde_json::{json, Value};
use crate::lifecycle::{self, Recovery};

pub fn exit_intent(state: &Path, launch_id: &str) {
    lifecycle::event(state, "desktop", "exit_requested", json!({"reason":"normal_exit", "launch_id":launch_id}));
    lifecycle::status(state, "desktop-exit-intent.json", json!({"launch_id":launch_id,"pid":std::process::id(),"reason":"normal_exit"}));
}

fn intended(state: &Path, launch_id: &str) -> bool {
    fs::read(state.join("desktop-exit-intent.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|record| record["launch_id"] == launch_id && record["reason"] == "normal_exit")
}

pub fn child_handshake(args: &[String]) -> Result<String, String> {
    let positions: Vec<usize> = args.iter().enumerate().filter_map(|(i, arg)| (arg == "--supervised-child").then_some(i)).collect();
    if positions.len() != 1 { return Err("Некорректный режим запуска окна".into()); }
    let id = args.get(positions[0] + 1).filter(|id| !id.is_empty() && id.len() <= 120 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .ok_or("Нет идентификатора запуска")?.clone();
    let mut start = [0u8];
    std::io::stdin().read_exact(&mut start).map_err(|_| "Наблюдатель запуска остановился")?;
    if start != [b'G'] { return Err("Неподтверждённый запуск окна".into()); }
    Ok(id)
}

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, os::windows::{io::AsRawHandle, process::CommandExt}, process::Child};
    use super::*;
    type Handle = *mut c_void;
    #[repr(C)]
    #[derive(Default)]
    struct Basic { process_time:i64, job_time:i64, flags:u32, min_ws:usize, max_ws:usize,
                   active:u32, affinity:usize, priority:u32, scheduling:u32 }
    #[repr(C)]
    #[derive(Default)]
    struct Extended { basic:Basic, io:[u64;6], process_memory:usize, job_memory:usize,
                      peak_process:usize, peak_job:usize }
    #[link(name="kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes:Handle, name:*const u16) -> Handle;
        fn SetInformationJobObject(job:Handle, class:i32, info:Handle, length:u32) -> i32;
        fn AssignProcessToJobObject(job:Handle, process:Handle) -> i32;
        fn CloseHandle(handle:Handle) -> i32;
        fn CreateMutexW(attributes:Handle, owner:i32, name:*const u16) -> Handle;
        fn ReleaseMutex(handle:Handle) -> i32;
        fn GetLastError() -> u32;
    }
    pub struct Job(Handle);
    impl Drop for Job { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    impl Job {
        pub fn own(child:&Child) -> Result<Self,String> {
            unsafe {
                let job = Self(CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()));
                if job.0.is_null() { return Err("Не удалось создать границу служб".into()); }
                let mut limits = Extended::default(); limits.basic.flags = 0x2000;
                if SetInformationJobObject(job.0, 9, &mut limits as *mut _ as Handle, std::mem::size_of::<Extended>() as u32) == 0
                    || AssignProcessToJobObject(job.0, child.as_raw_handle() as Handle) == 0 {
                    return Err("Windows не позволил изолировать службы".into());
                }
                Ok(job)
            }
        }
    }
    pub struct Owner(Handle);
    impl Drop for Owner { fn drop(&mut self) { unsafe { ReleaseMutex(self.0); CloseHandle(self.0); } } }
    pub fn acquire(state:&Path) -> Result<Option<Owner>,String> {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        fs::canonicalize(state).map_err(|_| "Папка состояния недоступна")?.to_string_lossy().to_lowercase().hash(&mut hash);
        let name:Vec<u16> = format!("Local\\LES-RAG-owner-{:016x}\0", hash.finish()).encode_utf16().collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null_mut(),1,name.as_ptr());
            if handle.is_null() { return Err("Не удалось проверить владельца запуска".into()); }
            if GetLastError() == 183 { CloseHandle(handle); return Ok(None); }
            Ok(Some(Owner(handle)))
        }
    }
    pub fn hidden(command:&mut Command) { command.creation_flags(0x08000000); }
}

#[cfg(windows)]
pub fn run(executable:&Path, state:&Path, args:&[String]) -> Result<(),String> {
    let Some(_owner) = windows::acquire(state)? else { return Ok(()); };
    if let Some(previous) = fs::read(state.join("supervisor-status.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if previous["phase"] == "running" {
            lifecycle::event(state,"supervisor","previous_owner_missing",json!({"previous_pid":previous["pid"],"reason":"previous_exit_unrecorded"}));
        }
    }
    let mut recovery = Recovery::default();
    loop {
        let launch_id = format!("{}-{}",std::process::id(),(lifecycle::stamp()*1_000_000.0) as u128);
        let mut command = Command::new(executable);
        command.args(&args[1..]).arg("--supervised-child").arg(&launch_id)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        windows::hidden(&mut command);
        let mut child = command.spawn().map_err(|_| "Не удалось открыть окно Леса")?;
        let job = match windows::Job::own(&child) {
            Ok(job) => job,
            Err(error) => { let _=child.kill(); let _=child.wait(); return Err(error); }
        };
        lifecycle::event(state,"supervisor","desktop_started",json!({"desktop_pid":child.id(),"launch_id":launch_id}));
        lifecycle::status(state,"supervisor-status.json",json!({"phase":"running","pid":std::process::id(),"desktop_pid":child.id(),"launch_id":launch_id,"updated_at":lifecycle::stamp()}));
        if child.stdin.take().ok_or("Нет канала запуска")?.write_all(b"G").is_err() {
            let _=child.kill(); let _=child.wait(); return Err("Окно не подтвердило запуск".into());
        }
        let result = child.wait().map_err(|_| "Не удалось прочитать причину остановки окна")?;
        let normal = intended(state,&launch_id);
        lifecycle::event(state,"supervisor","desktop_exited",json!({"desktop_pid":child.id(),"exit_code":result.code(),
            "reason":if normal {"normal_exit"} else {"unexpected_exit"},"launch_id":launch_id}));
        // Release all descendants before a replacement can touch the same DB.
        drop(job);
        if normal {
            lifecycle::status(state,"launcher-status.json",json!({"schema":"les.light-launcher.v1","phase":"stopped","message":"LES RAG остановлен","reason":"normal_exit","updated_at":lifecycle::stamp()}));
            lifecycle::status(state,"supervisor-status.json",json!({"phase":"stopped","pid":std::process::id(),"reason":"normal_exit","updated_at":lifecycle::stamp()}));
            return Ok(());
        }
        let Some(delay) = recovery.next(Instant::now()) else {
            lifecycle::event(state,"supervisor","recovery_limit",json!({"reason":"repeated_desktop_exit"}));
            lifecycle::status(state,"supervisor-status.json",json!({"phase":"error","reason":"recovery_limit","updated_at":lifecycle::stamp()}));
            lifecycle::status(state,"launcher-status.json",json!({"schema":"les.light-launcher.v1","phase":"error","message":"Окно повторно завершается. Автовосстановление приостановлено; откройте журнал запуска.","reason":"recovery_limit","updated_at":lifecycle::stamp()}));
            return Ok(());
        };
        lifecycle::status(state,"launcher-status.json",json!({"schema":"les.light-launcher.v1","phase":"recovering","message":"Окно остановилось. Восстанавливаю Лес…","reason":"unexpected_desktop_exit","updated_at":lifecycle::stamp()}));
        thread::sleep(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_exit_intent_cannot_disable_recovery_for_a_new_desktop() {
        let state=std::env::temp_dir().join(format!("les-intent-{}",std::process::id()));
        fs::create_dir_all(&state).unwrap(); exit_intent(&state,"old");
        assert!(intended(&state,"old")); assert!(!intended(&state,"new"));
        fs::remove_dir_all(state).unwrap();
    }
}
