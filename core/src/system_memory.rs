//! Match the application's admission floor using native OS counters.
pub fn available() -> Result<u64, &'static str> {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Status {
            length: u32,
            load: u32,
            total: u64,
            available: u64,
            total_page: u64,
            available_page: u64,
            total_virtual: u64,
            available_virtual: u64,
            available_extended: u64,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
        }
        let mut value = Status {
            length: std::mem::size_of::<Status>() as u32,
            load: 0,
            total: 0,
            available: 0,
            total_page: 0,
            available_page: 0,
            total_virtual: 0,
            available_virtual: 0,
            available_extended: 0,
        };
        if unsafe { GlobalMemoryStatusEx(&mut value) } == 0 {
            return Err("MEMORY_STATUS_UNAVAILABLE");
        }
        Ok(value.available)
    }
    #[cfg(target_os = "linux")]
    {
        let value =
            std::fs::read_to_string("/proc/meminfo").map_err(|_| "MEMORY_STATUS_UNAVAILABLE")?;
        value
            .lines()
            .find_map(|v| v.strip_prefix("MemAvailable:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .and_then(|v| v.checked_mul(1024))
            .ok_or("MEMORY_STATUS_UNAVAILABLE")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err("MEMORY_STATUS_UNAVAILABLE")
    }
}
