use anyhow::{Result, anyhow, ensure};
use sha3::{Digest, Keccak256};

const HEADER_SIZE: usize = 1;
const NONCE_SIZE: usize = 4;
const VERSION: u8 = 9;
const TYPE_ENTRY: u8 = 0x02;
const TYPE_UNZIPPED_PAYLOAD: u8 = 0x04;
const ENTRY_DATA_SIZE: usize = 28;
const MAX_L2_DATA_SIZE: usize = 24_800;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgraKaspaPayload {
    L2Submission {
        l2_tx_hash: [u8; 32],
    },
    Entry {
        recipient: [u8; 20],
        amount_sompi: u64,
    },
}

#[derive(Clone, Debug)]
pub struct IgraPayloadParser {
    txid_prefix: Vec<u8>,
}

impl IgraPayloadParser {
    pub fn new(txid_prefix_hex: &str) -> Result<Self> {
        let txid_prefix = hex::decode(txid_prefix_hex.trim_start_matches("0x"))
            .map_err(|e| anyhow!("invalid Kaspa txid prefix hex: {e}"))?;
        ensure!(
            !txid_prefix.is_empty(),
            "Kaspa txid prefix must not be empty"
        );
        ensure!(
            txid_prefix.len() <= 32,
            "Kaspa txid prefix cannot exceed 32 bytes"
        );
        Ok(Self { txid_prefix })
    }

    pub fn txid_prefix(&self) -> &[u8] {
        &self.txid_prefix
    }

    pub fn txid_prefix_hex(&self) -> String {
        hex::encode(&self.txid_prefix)
    }

    pub fn txid_matches(&self, txid: &[u8; 32]) -> bool {
        txid.starts_with(&self.txid_prefix)
    }

    pub fn parse(&self, txid: &[u8; 32], payload: &[u8]) -> Result<Option<IgraKaspaPayload>> {
        if !self.txid_matches(txid) {
            return Ok(None);
        }

        ensure!(
            payload.len() >= HEADER_SIZE + NONCE_SIZE,
            "Igra payload is too short"
        );

        let header = payload[0];
        let version = header >> 4;
        let tx_type = header & 0x0f;
        if version != VERSION {
            return Ok(None);
        }

        let l2_data = &payload[HEADER_SIZE..payload.len() - NONCE_SIZE];
        ensure!(
            l2_data.len() <= MAX_L2_DATA_SIZE,
            "Igra L2 payload exceeds max size"
        );

        match tx_type {
            TYPE_UNZIPPED_PAYLOAD => {
                let hash = Keccak256::digest(l2_data);
                let mut l2_tx_hash = [0u8; 32];
                l2_tx_hash.copy_from_slice(&hash);
                Ok(Some(IgraKaspaPayload::L2Submission { l2_tx_hash }))
            }
            TYPE_ENTRY => {
                ensure!(
                    l2_data.len() == ENTRY_DATA_SIZE,
                    "Igra entry payload must be 28 bytes"
                );
                let mut recipient = [0u8; 20];
                recipient.copy_from_slice(&l2_data[..20]);
                let amount_sompi = u64::from_le_bytes(l2_data[20..28].try_into()?);
                Ok(Some(IgraKaspaPayload::Entry {
                    recipient,
                    amount_sompi,
                }))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matching_txid() -> [u8; 32] {
        let mut txid = [0u8; 32];
        txid[0] = 0x97;
        txid[1] = 0xb1;
        txid
    }

    #[test]
    fn parses_l2_submission() {
        let parser = IgraPayloadParser::new("97b1").unwrap();
        let payload = [vec![0x94], b"hello igra".to_vec(), vec![0, 0, 0, 1]].concat();

        let parsed = parser.parse(&matching_txid(), &payload).unwrap();

        let expected = Keccak256::digest(b"hello igra");
        let mut expected_hash = [0u8; 32];
        expected_hash.copy_from_slice(&expected);
        assert_eq!(
            parsed,
            Some(IgraKaspaPayload::L2Submission {
                l2_tx_hash: expected_hash
            })
        );
    }

    #[test]
    fn parses_entry() {
        let parser = IgraPayloadParser::new("97b1").unwrap();
        let recipient = [0x11u8; 20];
        let amount = 123_456u64;
        let mut l2_data = Vec::from(recipient);
        l2_data.extend_from_slice(&amount.to_le_bytes());
        let payload = [vec![0x92], l2_data, vec![1, 2, 3, 4]].concat();

        let parsed = parser.parse(&matching_txid(), &payload).unwrap();

        assert_eq!(
            parsed,
            Some(IgraKaspaPayload::Entry {
                recipient,
                amount_sompi: amount
            })
        );
    }

    #[test]
    fn ignores_non_matching_txid_prefix() {
        let parser = IgraPayloadParser::new("97b1").unwrap();
        let txid = [0u8; 32];
        let payload = [vec![0x94], b"hello".to_vec(), vec![0, 0, 0, 0]].concat();

        assert_eq!(parser.parse(&txid, &payload).unwrap(), None);
    }

    #[test]
    fn ignores_wrong_version() {
        let parser = IgraPayloadParser::new("97b1").unwrap();
        let payload = [vec![0x84], b"hello".to_vec(), vec![0, 0, 0, 0]].concat();

        assert_eq!(parser.parse(&matching_txid(), &payload).unwrap(), None);
    }

    #[test]
    fn rejects_short_entry() {
        let parser = IgraPayloadParser::new("97b1").unwrap();
        let payload = [vec![0x92], vec![1, 2, 3], vec![0, 0, 0, 0]].concat();

        assert!(parser.parse(&matching_txid(), &payload).is_err());
    }
}
