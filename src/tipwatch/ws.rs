use anyhow::{Context, Result, anyhow, bail};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::debug;

use super::rpc::{Header, parse_header};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// An `eth_subscribe newHeads` subscription: every head the node switches to,
/// including versions that are replaced before any poll could see them.
pub struct HeadStream {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    id: String,
}

impl HeadStream {
    pub async fn connect(url: &str) -> Result<Self> {
        let (mut ws, _) =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, tokio_tungstenite::connect_async(url))
                .await
                .context("connecting timed out")?
                .context("connecting")?;
        ws.send(Message::text(
            json!({"jsonrpc": "2.0", "id": 1, "method": "eth_subscribe", "params": ["newHeads"]})
                .to_string(),
        ))
        .await
        .context("sending eth_subscribe")?;
        let id = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            loop {
                let msg = ws
                    .next()
                    .await
                    .ok_or_else(|| anyhow!("closed before answering eth_subscribe"))??;
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                if v.get("id") != Some(&json!(1)) {
                    continue;
                }
                if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                    bail!("eth_subscribe: RPC error {err}");
                }
                return v
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("eth_subscribe returned no subscription id"));
            }
        })
        .await
        .context("waiting for the eth_subscribe answer timed out")??;
        Ok(Self { ws, id })
    }

    /// The next pushed head. `Ok(None)` for anything else: pings, other
    /// subscriptions, notifications that don't parse. An error means the
    /// subscription is gone.
    pub async fn next(&mut self) -> Result<Option<Header>> {
        let msg = self
            .ws
            .next()
            .await
            .ok_or_else(|| anyhow!("connection closed"))?
            .context("reading")?;
        let text = match msg {
            Message::Text(text) => text,
            Message::Close(frame) => bail!("closed by the node: {frame:?}"),
            _ => return Ok(None),
        };
        let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
            debug!("ignoring a message that is not JSON");
            return Ok(None);
        };
        if v.get("method").and_then(Value::as_str) != Some("eth_subscription")
            || v["params"].get("subscription").and_then(Value::as_str) != Some(self.id.as_str())
        {
            return Ok(None);
        }
        match parse_header(v["params"]["result"].clone()) {
            Ok(Some(h)) => Ok(Some(h)),
            Ok(None) | Err(_) => {
                debug!("ignoring a new-heads notification without a usable header");
                Ok(None)
            }
        }
    }
}
