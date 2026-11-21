//! Content-free counters for a single response from a chat provider.
use serde_json::{json, Value};

#[derive(Default)]
pub(crate) struct StreamMetrics {
    pub wire_bytes: usize,
    pub lines: usize,
    pub content_bytes: usize,
    pub reasoning_bytes: usize,
    pub tool_bytes: usize,
    limit: Option<Value>,
}

impl StreamMetrics {
    pub fn payload_bytes(&self) -> usize {
        self.content_bytes + self.reasoning_bytes + self.tool_bytes
    }

    pub fn check_payload(&mut self, maximum: usize) -> Result<(), &'static str> {
        let size = self.payload_bytes();
        if size > maximum {
            Err(self.exceeded("response_payload_bytes", size, maximum))
        } else {
            Ok(())
        }
    }

    pub fn exceeded(&mut self, name: &str, observed: usize, maximum: usize) -> &'static str {
        self.limit = Some(json!({"name":name,"observed":observed,"maximum":maximum}));
        "UPSTREAM_RESPONSE_TOO_LARGE"
    }

    pub fn snapshot(&self, protocol: &str) -> Value {
        json!({"protocol":protocol,"wire_bytes_received":self.wire_bytes,"lines_processed":self.lines,
            "content_bytes":self.content_bytes,"separate_reasoning_bytes":self.reasoning_bytes,
            "tool_bytes":self.tool_bytes,"limit":self.limit})
    }
}
