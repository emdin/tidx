use chrono::{DateTime, TimeZone, Utc};

const BLOCK_COUNT_BITS: usize = 4;
const DAA_SCORE_BITS: usize = 38;
const FIRST_TIMESTAMP_DELTA_BITS: usize = 25;
const SUBSEQUENT_TIMESTAMP_DELTA_BITS: usize = 21;
const MAX_L1_BLOCK_COUNT: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgraTimestampMetadata {
    pub real_timestamp: DateTime<Utc>,
    pub real_timestamp_ms: i64,
    pub timestamp_drift_secs: i32,
    pub l1_block_count: i16,
    pub l1_last_daa_score: i64,
}

pub fn decode_igra_timestamp_metadata(
    parent_beacon_block_root: &[u8],
    evm_timestamp_secs: u64,
) -> Option<IgraTimestampMetadata> {
    if parent_beacon_block_root.len() != 32 || parent_beacon_block_root.iter().all(|b| *b == 0) {
        return None;
    }

    let block_count = read_bits(parent_beacon_block_root, 0, BLOCK_COUNT_BITS);
    if !(1..=MAX_L1_BLOCK_COUNT).contains(&block_count) {
        return None;
    }

    let last_daa_score = read_bits(parent_beacon_block_root, BLOCK_COUNT_BITS, DAA_SCORE_BITS);
    let first_delta_offset = BLOCK_COUNT_BITS + DAA_SCORE_BITS;
    let first_delta = sign_extend(
        read_bits(
            parent_beacon_block_root,
            first_delta_offset,
            FIRST_TIMESTAMP_DELTA_BITS,
        ),
        FIRST_TIMESTAMP_DELTA_BITS,
    );

    // The adapter sets the L2 block.timestamp to f(last_block_DAA), and encodes
    // real L1 timestamps as deltas from that same reference. Use the header value
    // directly so public-chain reference constants do not need to be duplicated here.
    let first_timestamp_secs = checked_add_signed(evm_timestamp_secs, first_delta)?;
    let mut real_timestamp_secs = first_timestamp_secs;

    let mut bit_offset = first_delta_offset + FIRST_TIMESTAMP_DELTA_BITS;
    for _ in 1..block_count {
        let delta = sign_extend(
            read_bits(
                parent_beacon_block_root,
                bit_offset,
                SUBSEQUENT_TIMESTAMP_DELTA_BITS,
            ),
            SUBSEQUENT_TIMESTAMP_DELTA_BITS,
        );
        real_timestamp_secs = checked_add_signed(first_timestamp_secs, delta)?;
        bit_offset += SUBSEQUENT_TIMESTAMP_DELTA_BITS;
    }

    let real_timestamp = Utc.timestamp_opt(real_timestamp_secs as i64, 0).single()?;
    let drift = (evm_timestamp_secs as i128) - (real_timestamp_secs as i128);
    let timestamp_drift_secs = drift.clamp(i32::MIN as i128, i32::MAX as i128) as i32;

    Some(IgraTimestampMetadata {
        real_timestamp,
        real_timestamp_ms: (real_timestamp_secs * 1000) as i64,
        timestamp_drift_secs,
        l1_block_count: block_count as i16,
        l1_last_daa_score: last_daa_score as i64,
    })
}

fn checked_add_signed(value: u64, delta: i64) -> Option<u64> {
    if delta >= 0 {
        value.checked_add(delta as u64)
    } else {
        value.checked_sub(delta.unsigned_abs())
    }
}

fn read_bits(bytes: &[u8], bit_offset: usize, num_bits: usize) -> u64 {
    let mut value = 0u64;
    for i in 0..num_bits {
        let bit_index = bit_offset + i;
        let byte_index = bit_index / 8;
        let bit_in_byte = 7 - (bit_index % 8);
        let bit = (bytes[byte_index] >> bit_in_byte) & 1;
        value = (value << 1) | bit as u64;
    }
    value
}

fn sign_extend(value: u64, bits: usize) -> i64 {
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bits(bytes: &mut [u8; 32], bit_offset: usize, value: u64, num_bits: usize) {
        for i in 0..num_bits {
            let bit_index = bit_offset + i;
            let byte_index = bit_index / 8;
            let bit_in_byte = 7 - (bit_index % 8);
            if (value >> (num_bits - 1 - i)) & 1 == 1 {
                bytes[byte_index] |= 1 << bit_in_byte;
            }
        }
    }

    fn pack(block_count: u8, daa_score: u64, first_delta: i64, deltas: &[i64]) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        let mut offset = 0;
        write_bits(
            &mut bytes,
            offset,
            block_count as u64 & 0x0f,
            BLOCK_COUNT_BITS,
        );
        offset += BLOCK_COUNT_BITS;
        write_bits(&mut bytes, offset, daa_score, DAA_SCORE_BITS);
        offset += DAA_SCORE_BITS;
        write_bits(
            &mut bytes,
            offset,
            first_delta as u64 & ((1u64 << FIRST_TIMESTAMP_DELTA_BITS) - 1),
            FIRST_TIMESTAMP_DELTA_BITS,
        );
        offset += FIRST_TIMESTAMP_DELTA_BITS;
        for delta in deltas {
            write_bits(
                &mut bytes,
                offset,
                *delta as u64 & ((1u64 << SUBSEQUENT_TIMESTAMP_DELTA_BITS) - 1),
                SUBSEQUENT_TIMESTAMP_DELTA_BITS,
            );
            offset += SUBSEQUENT_TIMESTAMP_DELTA_BITS;
        }
        bytes
    }

    #[test]
    fn decodes_last_l1_timestamp_from_beacon_root() {
        let daa_score = 410_181_447;
        let reference = 1_776_437_854;
        let root = pack(3, daa_score, -20, &[3, 8]);
        let metadata = decode_igra_timestamp_metadata(&root, reference + 100).unwrap();

        assert_eq!(metadata.l1_block_count, 3);
        assert_eq!(metadata.l1_last_daa_score, daa_score as i64);
        assert_eq!(
            metadata.real_timestamp.timestamp(),
            (reference + 100 - 20 + 8) as i64
        );
        assert_eq!(metadata.timestamp_drift_secs, 12);
    }

    #[test]
    fn ignores_empty_or_invalid_roots() {
        assert!(decode_igra_timestamp_metadata(&[0u8; 32], 1).is_none());
        assert!(decode_igra_timestamp_metadata(&[0u8; 31], 1).is_none());
    }
}
