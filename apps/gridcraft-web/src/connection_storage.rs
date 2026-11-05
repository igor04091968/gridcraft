//! Explicitly remembered pairing credentials, separate from ordinary UI preferences.

pub const KEY: &str = "gridcraft.connection";
const MAX_STORED: usize = 8192;

pub struct RememberedConnection {
    pub url: String,
    pub token: String,
}

impl RememberedConnection {
    pub fn restore(value: &str) -> Option<Self> {
        if value.len() > MAX_STORED {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(value).ok()?;
        let url = value.get("url")?.as_str()?;
        let token = value.get("token")?.as_str()?;
        if url.is_empty()
            || url.len() > 2048
            || url.chars().any(char::is_control)
            || token.len() < gridcraft_mcp::control_auth::MIN_TOKEN_LEN
            || token.len() > 1024
            || token.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self { url: url.into(), token: token.into() })
    }

    pub fn encode(url: &str, token: &str) -> Option<String> {
        let value = serde_json::json!({"url":url, "token":token}).to_string();
        Self::restore(&value).map(|_| value)
    }

    /// Never reuse a remembered credential for a different endpoint.
    pub fn token_for(&self, url: &str) -> Option<&str> {
        (self.url == url).then_some(self.token.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_roundtrip_is_bound_to_endpoint() {
        assert_ne!(KEY, crate::prefs::AGENT_KEY);
        let token = "a".repeat(64);
        let saved = RememberedConnection::encode("wss://example.test/agent/ws", &token).unwrap();
        let restored = RememberedConnection::restore(&saved).unwrap();
        assert_eq!(restored.token_for("wss://example.test/agent/ws"), Some(token.as_str()));
        assert_eq!(restored.token_for("wss://other.test/agent/ws"), None);
        assert_eq!(restored.token_for("wss://example.test/other"), None);
        assert!(!saved.contains("apiKey"));
    }

    #[test]
    fn corrupt_and_oversized_storage_is_ignored() {
        for value in ["", "null", "[]", "{}", "not json", r#"{"url":"wss://example.test","token":"short"}"#] {
            assert!(RememberedConnection::restore(value).is_none());
        }
        assert!(RememberedConnection::restore(&"x".repeat(MAX_STORED + 1)).is_none());
        assert!(RememberedConnection::encode("wss://example.test", &"a".repeat(1025)).is_none());
        assert!(RememberedConnection::encode("wss://example.test", &format!("{}\n", "a".repeat(64))).is_none());
    }
}
