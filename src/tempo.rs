//! Catch-all EVM/L2 RPC types.
//!
//! `AnyNetwork` preserves unknown/custom fields and non-mainnet transaction
//! shapes from reth-based L2s while still exposing the standard EVM fields
//! the indexer needs.

use std::ops::Deref;

use alloy::primitives::B256;
use serde::{Deserialize, Deserializer};

pub type AnyBlock = alloy::network::AnyRpcBlock;
pub type Transaction = alloy::network::AnyRpcTransaction;
pub type Log = alloy::rpc::types::Log;
pub type Receipt = alloy::network::AnyTransactionReceipt;

#[derive(Clone, Debug)]
pub struct Block {
    inner: AnyBlock,
    pub parent_beacon_block_root: Option<B256>,
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let parent_beacon_block_root = parent_beacon_block_root_from_value(&value)
            .map_err(|err| serde::de::Error::custom(err.to_string()))?;
        let inner = AnyBlock::deserialize(value).map_err(serde::de::Error::custom)?;

        Ok(Self {
            inner,
            parent_beacon_block_root,
        })
    }
}

impl Deref for Block {
    type Target = AnyBlock;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

fn parent_beacon_block_root_from_value(value: &serde_json::Value) -> anyhow::Result<Option<B256>> {
    let Some(raw) = value.get("parentBeaconBlockRoot") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }

    Ok(Some(serde_json::from_value(raw.clone())?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_parent_beacon_block_root() {
        let value = json!({
            "parentBeaconBlockRoot": "0x20061ce52bffffed800000000000000000000000000000000000000000000000",
        });

        let root = parent_beacon_block_root_from_value(&value)
            .unwrap()
            .unwrap();

        assert_eq!(
            root.to_string(),
            "0x20061ce52bffffed800000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn treats_missing_parent_beacon_block_root_as_none() {
        let value = json!({});

        assert!(
            parent_beacon_block_root_from_value(&value)
                .unwrap()
                .is_none()
        );
    }
}
