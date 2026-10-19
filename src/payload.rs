use serde_json::{Map, Value};

/// Event payload delivered over WebSocket.
///
/// `{"repository": "owner/repo", "event_name": "...", "event_action": "...", "issue_number": 1}`
#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub repository: String,
    pub event_name: String,
    pub event_action: String,
    pub issue_number: Option<u64>,
    /// All fields of the payload (used as built-in variables)
    pub fields: Map<String, Value>,
}

impl Payload {
    pub fn from_value(value: Value) -> Option<Self> {
        let Value::Object(fields) = value else {
            return None;
        };
        let string = |key: &str| fields.get(key).and_then(Value::as_str).map(str::to_string);
        let issue_number = match fields.get("issue_number") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        };
        Some(Self {
            repository: string("repository")?,
            event_name: string("event_name")?,
            event_action: string("event_action").unwrap_or_default(),
            issue_number,
            fields,
        })
    }

    /// HookType: `{event_name}:{event_action}`
    pub fn hook_type(&self) -> String {
        format!("{}:{}", self.event_name, self.event_action)
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

    if let Some(payload) = Payload::from_value(value.clone()) {
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
        if let Some(payload) = data.and_then(Payload::from_value) {
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
            "event_action": "opened",
            "issue_number": 3
        })
        .to_string();
        let Message::Payload(p) = parse_message(&text) else {
            panic!("not payload");
        };
        assert_eq!(p.repository, "owner/repo");
        assert_eq!(p.hook_type(), "issues:opened");
        assert_eq!(p.issue_number, Some(3));
    }

    #[test]
    fn parse_pusher_envelope() {
        let inner = json!({
            "repository": "owner/repo",
            "event_name": "issue_comment",
            "event_action": "created",
            "issue_number": "5"
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
        assert_eq!(p.issue_number, Some(5));
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
    fn payload_without_optional_fields() {
        let p = Payload::from_value(json!({ "repository": "o/r", "event_name": "push" })).unwrap();
        assert_eq!(p.hook_type(), "push:");
        assert_eq!(p.issue_number, None);
    }

    #[test]
    fn reject_payload_without_required_fields() {
        assert_eq!(Payload::from_value(json!({ "event_name": "push" })), None);
        assert_eq!(Payload::from_value(json!({ "repository": "o/r" })), None);
        assert_eq!(Payload::from_value(json!([])), None);
    }
}
