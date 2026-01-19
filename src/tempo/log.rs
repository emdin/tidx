use alloy::primitives::{Address, Bytes, B256, U64};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
    pub block_number: U64,
    pub block_hash: B256,
    pub transaction_hash: B256,
    pub transaction_index: U64,
    pub log_index: U64,
    #[serde(default)]
    pub removed: bool,
}

impl TempoLog {
    pub fn selector(&self) -> Option<&B256> {
        self.topics.first()
    }
}
