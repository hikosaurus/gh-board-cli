use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use crate::app::AppEvent;
use crate::payload::{Message, parse_message};

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Subscribe the WebSocket URL forever, reconnecting on failure.
pub async fn listen(url: String, channel: Option<String>, tx: UnboundedSender<AppEvent>) {
    loop {
        let _ = tx.send(AppEvent::Status(format!("connecting {url}")));
        if let Err(e) = session(&url, channel.as_deref(), &tx).await {
            let _ = tx.send(AppEvent::Status(format!("disconnected: {e}")));
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn session(
    url: &str,
    channel: Option<&str>,
    tx: &UnboundedSender<AppEvent>,
) -> anyhow::Result<()> {
    let (mut ws, _) = connect_async(url).await?;
    let _ = tx.send(AppEvent::Status("connected".into()));

    while let Some(msg) = ws.next().await {
        let text = match msg? {
            WsMessage::Text(text) => text,
            WsMessage::Close(_) => break,
            _ => continue,
        };
        match parse_message(&text) {
            Message::Payload(payload) => {
                let _ = tx.send(AppEvent::Received(payload));
            }
            Message::PusherConnected => {
                if let Some(channel) = channel {
                    let subscribe = json!({
                        "event": "pusher:subscribe",
                        "data": { "channel": channel }
                    });
                    ws.send(WsMessage::text(subscribe.to_string())).await?;
                    let _ = tx.send(AppEvent::Status(format!("subscribed {channel}")));
                }
            }
            Message::PusherPing => {
                let pong = json!({ "event": "pusher:pong", "data": {} });
                ws.send(WsMessage::text(pong.to_string())).await?;
            }
            Message::PusherControl(_) => {}
            Message::Unknown(text) => {
                let _ = tx.send(AppEvent::Status(format!("unknown message: {text}")));
            }
        }
    }
    anyhow::bail!("connection closed")
}
