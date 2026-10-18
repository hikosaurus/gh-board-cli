use serde::Deserialize;
use serde_json::Value;

/// Event payload delivered over WebSocket.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Payload {
    pub repository: String,
    pub event_name: String,
    #[serde(default)]
    pub event: Value,
}

impl Payload {
    pub fn action(&self) -> &str {
        self.event
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    /// HookType: `{event_name}:{event.action}`
    pub fn hook_type(&self) -> String {
        format!("{}:{}", self.event_name, self.action())
    }

    pub fn issue_number(&self) -> Option<u64> {
        self.event.pointer("/issue/number").and_then(Value::as_u64)
    }
}

/// Result of interpreting one WebSocket text message.
#[derive(Debug, PartialEq)]
pub enum Message {
    Payload(Payload),
    /// Pusher: connection established
    PusherConnected,
    /// Pusher: server ping (needs pong)
    PusherPing,
    /// Pusher: protocol message which needs no handling
    PusherControl(String),
    Unknown(String),
}

pub fn parse_message(text: &str) -> Message {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Message::Unknown(text.to_string());
    };

    if let Ok(payload) = serde_json::from_value::<Payload>(value.clone()) {
        return Message::Payload(payload);
    }

    // Pusher envelope: {"event": "...", "channel": "...", "data": "<json string>" | {...}}
    if let Some(event) = value.get("event").and_then(Value::as_str) {
        match event {
            "pusher:connection_established" => return Message::PusherConnected,
            "pusher:ping" => return Message::PusherPing,
            e if e.starts_with("pusher:") || e.starts_with("pusher_internal:") => {
                return Message::PusherControl(e.to_string());
            }
            _ => {}
        }
        let data = match value.get("data") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
            Some(v) => Some(v.clone()),
            None => None,
        };
        if let Some(payload) = data.and_then(|d| serde_json::from_value::<Payload>(d).ok()) {
            return Message::Payload(payload);
        }
    }

    Message::Unknown(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_raw_payload() {
        let text = json!({
            "repository": "owner/repo",
            "event_name": "issues",
            "event": { "action": "opened", "issue": { "number": 3 } }
        })
        .to_string();
        let Message::Payload(p) = parse_message(&text) else {
            panic!("not payload");
        };
        assert_eq!(p.repository, "owner/repo");
        assert_eq!(p.hook_type(), "issues:opened");
        assert_eq!(p.issue_number(), Some(3));
    }

    #[test]
    fn parse_pusher_envelope() {
        let inner = json!({
            "repository": "owner/repo",
            "event_name": "issue_comment",
            "event": { "action": "created" }
        });
        let text = json!({
            "event": "github",
            "channel": "ch",
            "data": inner.to_string()
        })
        .to_string();
        let Message::Payload(p) = parse_message(&text) else {
            panic!("not payload");
        };
        assert_eq!(p.hook_type(), "issue_comment:created");
    }

    #[test]
    fn parse_pusher_control() {
        assert_eq!(
            parse_message(r#"{"event":"pusher:connection_established","data":"{}"}"#),
            Message::PusherConnected
        );
        assert_eq!(
            parse_message(r#"{"event":"pusher:ping","data":{}}"#),
            Message::PusherPing
        );
    }

    #[test]
    fn hook_type_without_action() {
        let p = Payload {
            repository: "o/r".into(),
            event_name: "push".into(),
            event: json!({}),
        };
        assert_eq!(p.hook_type(), "push:");
    }
}
