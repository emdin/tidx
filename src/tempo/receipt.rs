use alloy::primitives::{Address, Bytes, B256, U64, U256};
use serde::{Deserialize, Serialize};

use super::TempoLog;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoReceipt {
    pub transaction_hash: B256,
    pub transaction_index: U64,
    pub block_hash: B256,
    pub block_number: U64,
    pub from: Address,
    #[serde(default)]
    pub to: Option<Address>,
    pub cumulative_gas_used: U64,
    pub gas_used: U64,
    #[serde(default)]
    pub contract_address: Option<Address>,
    pub logs: Vec<TempoLog>,
    #[serde(default)]
    pub status: Option<U64>,
    #[serde(default)]
    pub effective_gas_price: Option<U256>,
}
