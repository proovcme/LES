//! Normal close is a request; only the current renderer's saved acknowledgement exits.
#[derive(Default)]
pub struct CloseGuard {
    active: bool,
    sequence: u64,
    pending: Option<u64>,
}

impl CloseGuard {
    pub fn register(&mut self) { self.active = true; }
    pub fn invalidate(&mut self) { self.active = false; self.pending = None; }
    pub fn request(&mut self) -> Option<u64> {
        if !self.active { return None; }
        if self.pending.is_none() {
            self.sequence += 1;
            self.pending = Some(self.sequence);
        }
        self.pending
    }
    pub fn acknowledge(&mut self, nonce: u64, saved: bool) -> Result<bool, &'static str> {
        if !self.active || self.pending != Some(nonce) { return Err("Устаревшее подтверждение закрытия"); }
        self.pending = None;
        Ok(saved)
    }
}

pub fn owned_editor(actual: &tauri::Url, expected: &tauri::Url) -> bool {
    actual.scheme() == "http" && actual.host_str() == Some("127.0.0.1")
        && actual.port().is_some() && actual.origin() == expected.origin()
        && actual.path() == "/app/" && expected.path() == "/app/"
        && actual.username().is_empty() && actual.password().is_none()
}

pub fn editor_url(api: &str) -> Option<tauri::Url> {
    let mut url = tauri::Url::parse(api).ok()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none()
        || url.path() != "/" || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some() { return None; }
    url.set_path("/app/"); Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_close_waits_for_the_same_ack_and_failure_never_exits() {
        let mut guard = CloseGuard::default();
        assert_eq!(guard.request(), None);
        guard.register();
        let nonce = guard.request().unwrap();
        assert_eq!(guard.request(), Some(nonce));
        assert!(guard.acknowledge(nonce + 1, true).is_err());
        assert!(!guard.acknowledge(nonce, false).unwrap());
        let retry = guard.request().unwrap();
        assert_ne!(nonce, retry);
        assert!(guard.acknowledge(nonce, true).is_err());
        assert!(guard.acknowledge(retry, true).unwrap());
        assert!(guard.acknowledge(retry, true).is_err());
    }
    #[test]
    fn navigation_invalidates_prior_renderer_acknowledgements() {
        let mut guard = CloseGuard::default(); guard.register();
        let old = guard.request().unwrap(); guard.invalidate(); guard.register();
        let new = guard.request().unwrap();
        assert_ne!(old, new); assert!(guard.acknowledge(old, true).is_err());
        assert!(guard.acknowledge(new, true).unwrap());
    }
    #[test]
    fn only_the_owned_chat_can_acknowledge() {
        let expected = tauri::Url::parse("http://127.0.0.1:54001/app/").unwrap();
        for url in ["http://127.0.0.1:54002/app/", "http://127.0.0.1:54001/classic", "http://127.0.0.1:54001/api/documents/by-id/a/viewer", "https://example.org/app/"] {
            assert!(!owned_editor(&tauri::Url::parse(url).unwrap(), &expected));
        }
        assert!(owned_editor(&tauri::Url::parse("http://127.0.0.1:54001/app/?session=demo").unwrap(), &expected));
    }
}
