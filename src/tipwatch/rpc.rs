use alloy::primitives::B256;
use anyhow::Result;
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

/// JSON-RPC client for one provider, paced to at most `max_rps` requests per
/// second across all calls.
pub struct TipRpc {
    client: reqwest::Client,
    url: String,
    interval: Duration,
    next_slot: Mutex<Instant>,
}

impl TipRpc {
    pub fn new(url: &str, max_rps: f64) -> Result<Self> {
        let _ = (url, max_rps, &Duration::ZERO);
        todo!()
    }

    pub async fn chain_id(&self) -> Result<u64> {
        todo!()
    }

    pub async fn head(&self) -> Result<Header> {
        todo!()
    }

    pub async fn by_number(&self, number: u64) -> Result<Option<Header>> {
        let _ = number;
        todo!()
    }

    pub async fn by_hash(&self, hash: B256) -> Result<Option<Header>> {
        let _ = hash;
        todo!()
    }

    #[allow(dead_code)]
    fn fields(&self) -> (&reqwest::Client, &str, Duration, &Mutex<Instant>) {
        (&self.client, &self.url, self.interval, &self.next_slot)
    }
}
