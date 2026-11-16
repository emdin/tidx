use anyhow::{Context, Result, anyhow, bail};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, warn};

use super::rpc::{Header, parse_header};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The WebSocket endpoint is on another chain than the HTTP one. A
/// configuration error: retrying does not help.
#[derive(Debug, thiserror::Error)]
#[error("WebSocket endpoint is on chain {got}, the HTTP endpoint on chain {want}")]
pub struct WrongChain {
    pub got: u64,
    pub want: u64,
}

/// An `eth_subscribe newHeads` subscription: every head the node switches to,
/// including versions that are replaced before any poll could see them.
pub struct HeadStream {
    ws: Ws,
    id: String,
    unusable: u64,
}

/// Sends a JSON-RPC request and waits for its answer, skipping anything else.
async fn request(ws: &mut Ws, id: u64, method: &str, params: Value) -> Result<Value> {
    ws.send(Message::text(
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    ))
    .await
    .with_context(|| format!("sending {method}"))?;
    loop {
        let msg = ws
            .next()
            .await
            .ok_or_else(|| anyhow!("closed before answering {method}"))??;
        let Message::Text(text) = msg else { continue };
        let Ok(mut v) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        if v.get("id") != Some(&json!(id)) {
            continue;
        }
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            bail!("{method}: RPC error {err}");
        }
        return Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null));
    }
}

impl HeadStream {
    /// Connects, checks the endpoint is on `chain_id`, and subscribes. The
    /// whole handshake must finish within `timeout`.
    pub async fn connect(url: &str, chain_id: u64, timeout: Duration) -> Result<Self> {
        tokio::time::timeout(timeout, async {
            let (mut ws, _) = tokio_tungstenite::connect_async(url)
                .await
                .context("connecting")?;
            let got = request(&mut ws, 1, "eth_chainId", json!([])).await?;
            let got = got
                .as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .ok_or_else(|| anyhow!("eth_chainId returned {got}"))?;
            if got != chain_id {
                return Err(WrongChain {
                    got,
                    want: chain_id,
                }
                .into());
            }
            let id = request(&mut ws, 2, "eth_subscribe", json!(["newHeads"])).await?;
            let id = id
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("eth_subscribe returned no subscription id"))?;
            Ok(Self {
                ws,
                id,
                unusable: 0,
            })
        })
        .await
        .map_err(|_| anyhow!("subscribing took longer than {timeout:?}"))?
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
        let Ok(mut v) = serde_json::from_str::<Value>(text.as_str()) else {
            debug!("ignoring a message that is not JSON");
            return Ok(None);
        };
        if v.get("method").and_then(Value::as_str) != Some("eth_subscription")
            || v["params"].get("subscription").and_then(Value::as_str) != Some(self.id.as_str())
        {
            return Ok(None);
        }
        let result = v
            .pointer_mut("/params/result")
            .map(Value::take)
            .unwrap_or(Value::Null);
        match parse_header(result) {
            Ok(Some(h)) => Ok(Some(h)),
            other => {
                // A node whose header shape differs would otherwise look like
                // a silent subscription.
                self.unusable += 1;
                if self.unusable == 1 || self.unusable % 100 == 0 {
                    let why = match other {
                        Err(e) => format!("{e:#}"),
                        _ => "no header".to_string(),
                    };
                    warn!(
                        count = self.unusable,
                        why, "new-heads notification without a usable header"
                    );
                }
                Ok(None)
            }
        }
    }
}
