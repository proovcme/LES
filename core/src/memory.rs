//! Resource reclamation affects only our idle worker and unused heaps, never guards.
use serde_json::{json, Value};
pub fn execute(request: &Value) -> Result<Value, &'static str> {
    match request["action"].as_str() {
        Some("plan") => {
            let free = request["free_gb"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.)
                .ok_or("MEMORY_METRICS_INVALID")?;
            let required = request["required_gb"]
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.)
                .ok_or("MEMORY_METRICS_INVALID")?;
            let phase = request["phase"].as_str().ok_or("MEMORY_PHASE_INVALID")?;
            if !["batch_end", "queue_end", "chat_end", "chat_prepare"].contains(&phase) {
                return Err("MEMORY_PHASE_INVALID");
            }
            let pressure = free < required;
            Ok(
                json!({"pressure":pressure,"release_worker":phase=="queue_end"||pressure,
                "release_model":pressure&&((phase=="chat_end"&&request["owned_local_ollama"]==true)
                    ||(phase=="chat_prepare"&&request["planned_local_ollama"]==true))}),
            )
        }
        Some("reclaim") => {
            let worker = if request["release_worker"] == true {
                crate::worker_host::release_idle()?
            } else {
                json!({"released":false})
            };
            Ok(json!({"worker":worker,"heap_optimized":optimize_heap()}))
        }
        _ => Err("MEMORY_ACTION_INVALID"),
    }
}
#[cfg(windows)]
fn optimize_heap() -> bool {
    #[repr(C)]
    struct Info {
        version: u32,
        flags: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn HeapSetInformation(
            heap: *mut std::ffi::c_void,
            class: u32,
            info: *const Info,
            length: usize,
        ) -> i32;
    }
    let info = Info {
        version: 1,
        flags: 0,
    };
    // HeapOptimizeResources decommits unused LFH caches; it does not evict live
    // working-set pages or alter OOM/security policy. Windows 8.1 and later.
    unsafe { HeapSetInformation(std::ptr::null_mut(), 3, &info, std::mem::size_of::<Info>()) != 0 }
}
#[cfg(not(windows))]
fn optimize_heap() -> bool {
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_is_scoped_and_does_not_change_thresholds() {
        for free in [2., 4., 8.] {
            let r=execute(&json!({"action":"plan","phase":"chat_end","free_gb":free,"required_gb":4.,"owned_local_ollama":false})).unwrap();
            assert_eq!(r["release_model"], false);
        }
        assert_eq!(execute(&json!({"action":"plan","phase":"chat_end","free_gb":3.,"required_gb":4.,"owned_local_ollama":true})).unwrap()["release_model"],true);
        for planned in [false, true] {
            assert_eq!(execute(&json!({"action":"plan","phase":"chat_prepare","free_gb":3.,"required_gb":4.,"planned_local_ollama":planned})).unwrap()["release_model"],planned);
        }
    }
}
