//! Own exactly one spawned worker and its descendants; never enumerate by name.
use std::process::{Child, Command};

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::c_void,
        os::windows::{io::AsRawHandle, process::CommandExt},
    };
    type Handle = *mut c_void;
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_ws: usize,
        max_ws: usize,
        active: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Extended {
        basic: Basic,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process: usize,
        peak_job: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: Handle, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: Handle, length: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }
    pub struct JobGuard(Handle);
    // Win32 job handles may be transferred between threads; there is one owner.
    unsafe impl Send for JobGuard {}
    impl Drop for JobGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    impl JobGuard {
        pub fn own(child: &Child, memory_bytes: Option<usize>) -> Result<Self, &'static str> {
            unsafe {
                let job = Self(CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()));
                if job.0.is_null() {
                    return Err("WORKER_JOB_UNAVAILABLE");
                }
                let mut limits = Extended::default();
                limits.basic.flags = 0x2000;
                if let Some(bytes) = memory_bytes {
                    limits.basic.flags |= 0x200 | 0x8;
                    limits.job_memory = bytes;
                    limits.basic.active = 16;
                }
                if SetInformationJobObject(
                    job.0,
                    9,
                    &mut limits as *mut _ as Handle,
                    std::mem::size_of::<Extended>() as u32,
                ) == 0
                    || AssignProcessToJobObject(job.0, child.as_raw_handle() as Handle) == 0
                {
                    return Err("WORKER_JOB_ASSIGNMENT_FAILED");
                }
                Ok(job)
            }
        }
    }
    pub fn prepare(command: &mut Command) {
        command.creation_flags(0x08000000);
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    pub struct JobGuard(u32);
    impl Drop for JobGuard {
        fn drop(&mut self) {
            unsafe {
                kill(-(self.0 as i32), 9);
            }
        }
    }
    impl JobGuard {
        pub fn own(child: &Child, _memory_bytes: Option<usize>) -> Result<Self, &'static str> {
            Ok(Self(child.id()))
        }
    }
    pub fn prepare(command: &mut Command) {
        command.process_group(0);
    }
}

pub use platform::{prepare, JobGuard};
