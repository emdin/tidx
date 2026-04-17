//! Catch-all EVM/L2 RPC types.
//!
//! `AnyNetwork` preserves unknown/custom fields and non-mainnet transaction
//! shapes from reth-based L2s while still exposing the standard EVM fields
//! the indexer needs.

use std::ops::Deref;

use alloy::primitives::B256;
use serde::Deserialize;

pub type AnyBlock = alloy::network::AnyRpcBlock;
pub type Transaction = alloy::network::AnyRpcTransaction;
pub type Log = alloy::rpc::types::Log;
pub type Receipt = alloy::network::AnyTransactionReceipt;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    #[serde(flatten)]
    inner: AnyBlock,
    #[serde(default)]
    pub parent_beacon_block_root: Option<B256>,
}

impl Deref for Block {
    type Target = AnyBlock;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
