//! Catch-all EVM/L2 RPC types.
//!
//! `AnyNetwork` preserves unknown/custom fields and non-mainnet transaction
//! shapes from reth-based L2s while still exposing the standard EVM fields
//! the indexer needs.

pub type Block = alloy::network::AnyRpcBlock;
pub type Transaction = alloy::network::AnyRpcTransaction;
pub type Log = alloy::rpc::types::Log;
pub type Receipt = alloy::network::AnyTransactionReceipt;
