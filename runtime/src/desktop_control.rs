//! File rendezvous between the local CLI and the desktop that owns the profile.
use std::path::{Path, PathBuf};

pub const PROTOCOL: u64 = 1;

pub fn stop_marker(state: &Path, launch: &str) -> Option<PathBuf> {
    let (pid, stamp) = launch.split_once('-')?;
    if pid.parse::<u32>().ok()? == 0
        || stamp.parse::<u128>().ok()? == 0
        || !launch.bytes().all(|c| c.is_ascii_digit() || c == b'-')
    {
        return None;
    }
    Some(state.join(format!("desktop-stop-{launch}.request")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_bound_to_one_launch_and_never_escapes_state() {
        let root = Path::new("isolated");
        assert_ne!(stop_marker(root, "10-101"), stop_marker(root, "10-102"));
        assert_eq!(stop_marker(root, "10-101").unwrap().parent(), Some(root));
        for invalid in ["../foreign", "10-20-30", "0-12", "1-0", "1-/2", "+1-2"] {
            assert!(stop_marker(root, invalid).is_none());
        }
    }
}
