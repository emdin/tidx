use alloy::primitives::{Address, Bytes, B256, B64, U256, U64};
use serde::{Deserialize, Serialize};

use super::TempoTransaction;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoBlock {
    pub number: U64,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: U64,
    #[serde(default)]
    pub timestamp_millis: Option<U64>,
    #[serde(default)]
    pub timestamp_millis_part: Option<U64>,
    pub gas_limit: U64,
    pub gas_used: U64,
    pub miner: Address,
    #[serde(default)]
    pub extra_data: Option<Bytes>,
    #[serde(default)]
    pub base_fee_per_gas: Option<U256>,
    #[serde(default)]
    pub nonce: Option<B64>,
    #[serde(default)]
    pub transactions: TempoBlockTransactions,

    // Tempo-specific fields
    #[serde(default)]
    pub main_block_general_gas_limit: Option<U64>,
    #[serde(default)]
    pub shared_gas_limit: Option<U64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TempoBlockTransactions {
    Hashes(Vec<B256>),
    Full(Vec<TempoTransaction>),
}

impl Default for TempoBlockTransactions {
    fn default() -> Self {
        Self::Hashes(vec![])
    }
}

impl TempoBlock {
    pub fn number_u64(&self) -> u64 {
        self.number.to::<u64>()
    }

    pub fn timestamp_u64(&self) -> u64 {
        self.timestamp.to::<u64>()
    }

    pub fn timestamp_millis_u64(&self) -> u64 {
        if let Some(millis) = self.timestamp_millis {
            return millis.to::<u64>();
        }
        if let Some(part) = self.timestamp_millis_part {
            return self.timestamp_u64() * 1000 + part.to::<u64>();
        }
        self.timestamp_u64() * 1000
    }

    pub fn gas_limit_u64(&self) -> u64 {
        self.gas_limit.to::<u64>()
    }

    pub fn gas_used_u64(&self) -> u64 {
        self.gas_used.to::<u64>()
    }

    pub fn transactions(&self) -> impl Iterator<Item = &TempoTransaction> {
        match &self.transactions {
            TempoBlockTransactions::Full(txs) => txs.iter(),
            TempoBlockTransactions::Hashes(_) => [].iter(),
        }
    }
}
