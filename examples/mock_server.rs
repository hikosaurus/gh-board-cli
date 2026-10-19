//! Pusher-like mock WebSocket server for local development.
//!
//! usage: cargo run --example mock_server -- <owner>/<repo> [port]
//! then set `listen: ws://127.0.0.1:<port>` in `.ghboard.yaml`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let repository = args.next().unwrap_or_else(|| "owner/repo".into());
    let port = args.next().unwrap_or_else(|| "8765".into());
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    println!("listening ws://127.0.0.1:{port}");

    while let Ok((stream, _)) = listener.accept().await {
        let repository = repository.clone();
        tokio::spawn(async move {
            let Ok(mut ws) = accept_async(stream).await else {
                return;
            };
            let established = json!({
                "event": "pusher:connection_established",
                "data": json!({ "socket_id": "1.1" }).to_string()
            });
            if ws
                .send(Message::text(established.to_string()))
                .await
                .is_err()
            {
                return;
            }
            if let Some(Ok(msg)) = ws.next().await {
                println!("received: {msg}");
            }

            let events = [
                ("issues", "opened"),
                ("issue_comment", "created"),
                ("issues", "labeled"),
            ];
            for (event_name, event_action) in events {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let payload = json!({
                    "repository": repository,
                    "event_name": event_name,
                    "event_action": event_action,
                    "issue_number": 1
                });
                let envelope = json!({
                    "event": "github",
                    "channel": "gh-board",
                    "data": payload.to_string()
                });
                if ws.send(Message::text(envelope.to_string())).await.is_err() {
                    return;
                }
            }
            // keep the connection open
            while let Some(Ok(_)) = ws.next().await {}
        });
    }
    Ok(())
}
