//! Authentication refusals in the shared protocol deliberately omit a request id.
use serde_json::Value;

pub fn auth_outcome(response: &Value, authenticated: bool) -> Option<bool> {
    if response.get("method").is_some() {
        return None;
    }
    if response.get("id").and_then(Value::as_u64) == Some(0) || (!authenticated && response.get("id").is_none()) {
        response.get("ok").and_then(Value::as_bool)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use gridcraft_mcp::control_auth::{auth_line, check_first_line};
    use serde_json::json;

    #[test]
    fn shared_protocol_refusal_without_id_stops_authentication() {
        let refusal = check_first_line(&auth_line(0, "incorrect-token"), "correct-token").unwrap_err();
        assert!(refusal.get("id").is_none());
        assert_eq!(auth_outcome(&refusal, false), Some(false));
        let accepted = check_first_line(&auth_line(0, "correct-token"), "correct-token").unwrap();
        assert_eq!(auth_outcome(&accepted, false), Some(true));
    }

    #[test]
    fn workbook_responses_and_requests_are_not_authentication() {
        assert_eq!(auth_outcome(&json!({"id":"agent-1","ok":false}), false), None);
        assert_eq!(auth_outcome(&json!({"id":9000,"ok":false}), false), None);
        assert_eq!(auth_outcome(&json!({"ok":false}), true), None);
        assert_eq!(auth_outcome(&json!({"id":0,"method":"auth","ok":true}), false), None);
    }
}
