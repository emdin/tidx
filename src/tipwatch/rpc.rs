use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// The block fields the watcher needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: u64,
}

#[derive(Deserialize)]
struct RawHeader {
    number: String,
    hash: B256,
    #[serde(rename = "parentHash")]
    parent_hash: B256,
    timestamp: String,
}

fn quantity(field: &str, s: &str) -> Result<u64> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .with_context(|| format!("bad {field} {s:?}"))
}

fn parse_header(v: Value) -> Result<Option<Header>> {
    if v.is_null() {
        return Ok(None);
    }
    let raw: RawHeader = serde_json::from_value(v).context("unexpected block shape")?;
    Ok(Some(Header {
        number: quantity("number", &raw.number)?,
        hash: raw.hash,
        parent_hash: raw.parent_hash,
        timestamp: quantity("timestamp", &raw.timestamp)?,
    }))
}

/// JSON-RPC client for one provider, paced to at most `max_rps` requests per
/// second across all calls (the providers are public endpoints run by others).
pub struct TipRpc {
    client: reqwest::Client,
    url: String,
    interval: Duration,
    next_slot: Mutex<Instant>,
}

impl TipRpc {
    pub fn new(url: &str, max_rps: f64) -> Result<Self> {
        if !(max_rps > 0.0 && max_rps.is_finite()) {
            bail!("max_rps must be a positive number, got {max_rps}");
        }
        let client = reqwest::Client::builder()
            .gzip(true)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            url: url.to_string(),
            interval: Duration::from_secs_f64(1.0 / max_rps),
            next_slot: Mutex::new(Instant::now()),
        })
    }

    async fn pace(&self) {
        let mut next = self.next_slot.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + self.interval;
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.pace().await;
        let resp = self
            .client
            .post(&self.url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .with_context(|| format!("{method}: request failed"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .with_context(|| format!("{method}: reading body"))?;
        if !status.is_success() {
            bail!("{method}: HTTP {}", status.as_u16());
        }
        let mut v: Value = serde_json::from_str(&body)
            .map_err(|e| anyhow!("{method}: invalid JSON response: {e}"))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            bail!("{method}: RPC error {err}");
        }
        Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null))
    }

    pub async fn chain_id(&self) -> Result<u64> {
        let v = self.call("eth_chainId", json!([])).await?;
        quantity(
            "chain id",
            v.as_str()
                .ok_or_else(|| anyhow!("eth_chainId returned {v}"))?,
        )
    }

    pub async fn head(&self) -> Result<Header> {
        parse_header(
            self.call("eth_getBlockByNumber", json!(["latest", false]))
                .await?,
        )?
        .ok_or_else(|| anyhow!("eth_getBlockByNumber(latest) returned no block"))
    }

    pub async fn by_number(&self, number: u64) -> Result<Option<Header>> {
        let h = parse_header(
            self.call(
                "eth_getBlockByNumber",
                json!([format!("0x{number:x}"), false]),
            )
            .await?,
        )?;
        if let Some(h) = h
            && h.number != number
        {
            bail!("asked for block {number}, got block {}", h.number);
        }
        Ok(h)
    }

    pub async fn by_hash(&self, hash: B256) -> Result<Option<Header>> {
        let h = parse_header(
            self.call("eth_getBlockByHash", json!([hash, false]))
                .await?,
        )?;
        if let Some(h) = h
            && h.hash != hash
        {
            bail!("asked for {hash}, got block {} {}", h.number, h.hash);
        }
        Ok(h)
    }
}
