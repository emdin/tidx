use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Duration, Utc};

use crate::config::KaspaConfig;
use crate::db::Pool;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingL2Submission {
    pub l2_tx_hash: [u8; 32],
    pub kaspa_txid: [u8; 32],
    pub accepted_chain_block_hash: [u8; 32],
    pub accepted_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEntry {
    pub kaspa_txid: [u8; 32],
    pub recipient: [u8; 20],
    pub amount_sompi: u64,
    pub accepted_chain_block_hash: [u8; 32],
    pub accepted_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalL2Submission {
    pub l2_tx_hash: [u8; 32],
    pub kaspa_txid: [u8; 32],
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalEntry {
    pub kaspa_txid: [u8; 32],
    pub recipient: [u8; 20],
    pub amount_sompi: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromotedRows {
    pub l2_submissions: Vec<FinalL2Submission>,
    pub entries: Vec<FinalEntry>,
}

#[derive(Clone, Debug)]
pub struct KaspaSyncState {
    pub checkpoint_hash: Option<[u8; 32]>,
    pub tip_distance: u64,
}

#[derive(Clone)]
pub struct KaspaProvenanceWriter {
    pool: Pool,
    promotion_delay: Duration,
}

impl KaspaProvenanceWriter {
    pub fn new(pool: Pool, promotion_delay_secs: u64) -> Result<Self> {
        let secs = i64::try_from(promotion_delay_secs)
            .context("promotion_delay_secs does not fit in i64")?;
        Ok(Self {
            pool,
            promotion_delay: Duration::seconds(secs),
        })
    }

    pub async fn ensure_meta(
        &self,
        chain_id: u64,
        kaspa: &KaspaConfig,
        txid_prefix: &[u8],
    ) -> Result<()> {
        let chain_id_i64 = i64::try_from(chain_id).context("chain_id does not fit in i64")?;
        let promotion_delay_secs = i64::try_from(kaspa.promotion_delay_secs)
            .context("promotion_delay_secs does not fit in i64")?;
        let conn = self.pool.get().await?;
        let row = conn
            .query_opt(
                "SELECT chain_id, txid_prefix FROM kaspa_provenance_meta WHERE id = TRUE",
                &[],
            )
            .await?;

        if let Some(row) = row {
            let stored_chain_id: i64 = row.get(0);
            let stored_prefix: Vec<u8> = row.get(1);
            ensure!(
                stored_chain_id == chain_id_i64,
                "kaspa_provenance_meta chain_id mismatch: stored={stored_chain_id}, configured={chain_id_i64}"
            );
            ensure!(
                stored_prefix == txid_prefix,
                "kaspa_provenance_meta txid_prefix mismatch: stored={}, configured={}",
                hex::encode(stored_prefix),
                hex::encode(txid_prefix)
            );
            conn.execute(
                "UPDATE kaspa_provenance_meta
                 SET kaspa_rpc_url = $1, promotion_delay_secs = $2, updated_at = now()
                 WHERE id = TRUE",
                &[&kaspa.rpc_url, &promotion_delay_secs],
            )
            .await?;
        } else {
            conn.execute(
                "INSERT INTO kaspa_provenance_meta
                    (id, chain_id, kaspa_rpc_url, txid_prefix, promotion_delay_secs)
                 VALUES (TRUE, $1, $2, $3, $4)",
                &[
                    &chain_id_i64,
                    &kaspa.rpc_url,
                    &txid_prefix,
                    &promotion_delay_secs,
                ],
            )
            .await?;
        }
        Ok(())
    }

    pub async fn load_state(&self, default_tip_distance: u64) -> Result<KaspaSyncState> {
        let conn = self.pool.get().await?;
        let row = conn
            .query_opt(
                "SELECT checkpoint_hash, tip_distance FROM kaspa_sync_state WHERE id = TRUE",
                &[],
            )
            .await?;

        let Some(row) = row else {
            let default_tip_distance_i64 =
                i64::try_from(default_tip_distance).context("tip_distance does not fit in i64")?;
            conn.execute(
                "INSERT INTO kaspa_sync_state (id, tip_distance) VALUES (TRUE, $1)
                 ON CONFLICT (id) DO NOTHING",
                &[&default_tip_distance_i64],
            )
            .await?;
            return Ok(KaspaSyncState {
                checkpoint_hash: None,
                tip_distance: default_tip_distance,
            });
        };

        let checkpoint_hash = row.get::<_, Option<Vec<u8>>>(0).map(|bytes| {
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            out
        });
        let tip_distance = u64::try_from(row.get::<_, i64>(1)).unwrap_or(default_tip_distance);
        Ok(KaspaSyncState {
            checkpoint_hash,
            tip_distance,
        })
    }

    pub async fn update_success(
        &self,
        checkpoint_hash: &[u8; 32],
        sink: &[u8; 32],
        virtual_daa_score: Option<u64>,
        tip_distance: u64,
    ) -> Result<()> {
        let conn = self.pool.get().await?;
        let virtual_daa_score = virtual_daa_score
            .map(i64::try_from)
            .transpose()
            .context("virtual DAA score does not fit in i64")?;
        let tip_distance =
            i64::try_from(tip_distance).context("tip_distance does not fit in i64")?;
        conn.execute(
            "INSERT INTO kaspa_sync_state
                (id, checkpoint_hash, last_seen_sink, last_virtual_daa_score, tip_distance, last_success_at, last_error, updated_at)
             VALUES (TRUE, $1, $2, $3, $4, now(), NULL, now())
             ON CONFLICT (id) DO UPDATE SET
                checkpoint_hash = EXCLUDED.checkpoint_hash,
                last_seen_sink = EXCLUDED.last_seen_sink,
                last_virtual_daa_score = EXCLUDED.last_virtual_daa_score,
                tip_distance = EXCLUDED.tip_distance,
                last_success_at = now(),
                last_error = NULL,
                updated_at = now()",
            &[&checkpoint_hash.as_slice(), &sink.as_slice(), &virtual_daa_score, &tip_distance],
        )
        .await?;
        Ok(())
    }

    pub async fn record_error(&self, error: &str) -> Result<()> {
        let conn = self.pool.get().await?;
        conn.execute(
            "INSERT INTO kaspa_sync_state (id, last_error, updated_at)
             VALUES (TRUE, $1, now())
             ON CONFLICT (id) DO UPDATE SET last_error = EXCLUDED.last_error, updated_at = now()",
            &[&error],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_pending(
        &self,
        l2_submissions: &[PendingL2Submission],
        entries: &[PendingEntry],
    ) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        for row in l2_submissions {
            let promote_after = row.accepted_at + self.promotion_delay;
            tx.execute(
                "INSERT INTO kaspa_pending_l2_submissions
                    (l2_tx_hash, kaspa_txid, accepted_chain_block_hash, accepted_at, promote_after)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT DO NOTHING",
                &[
                    &row.l2_tx_hash.as_slice(),
                    &row.kaspa_txid.as_slice(),
                    &row.accepted_chain_block_hash.as_slice(),
                    &row.accepted_at,
                    &promote_after,
                ],
            )
            .await?;
        }

        for row in entries {
            let promote_after = row.accepted_at + self.promotion_delay;
            let amount_sompi =
                i64::try_from(row.amount_sompi).context("amount_sompi does not fit in i64")?;
            tx.execute(
                "INSERT INTO kaspa_pending_entries
                    (kaspa_txid, recipient, amount_sompi, accepted_chain_block_hash, accepted_at, promote_after)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT DO NOTHING",
                &[
                    &row.kaspa_txid.as_slice(),
                    &row.recipient.as_slice(),
                    &amount_sompi,
                    &row.accepted_chain_block_hash.as_slice(),
                    &row.accepted_at,
                    &promote_after,
                ],
            )
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_pending_for_removed_blocks(
        &self,
        block_hashes: &[[u8; 32]],
    ) -> Result<u64> {
        if block_hashes.is_empty() {
            return Ok(0);
        }

        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut deleted = 0;
        for hash in block_hashes {
            deleted += tx
                .execute(
                    "DELETE FROM kaspa_pending_l2_submissions WHERE accepted_chain_block_hash = $1",
                    &[&hash.as_slice()],
                )
                .await?;
            deleted += tx
                .execute(
                    "DELETE FROM kaspa_pending_entries WHERE accepted_chain_block_hash = $1",
                    &[&hash.as_slice()],
                )
                .await?;
        }
        tx.commit().await?;
        Ok(deleted)
    }

    pub async fn promote_due(&self) -> Result<PromotedRows> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        let l2_rows = tx
            .query(
                "WITH due AS (
                    SELECT l2_tx_hash, kaspa_txid
                    FROM kaspa_pending_l2_submissions
                    WHERE promote_after <= now()
                 ),
                 inserted AS (
                    INSERT INTO kaspa_l2_submissions (l2_tx_hash, kaspa_txid)
                    SELECT l2_tx_hash, kaspa_txid FROM due
                    ON CONFLICT DO NOTHING
                    RETURNING l2_tx_hash, kaspa_txid, created_at
                 ),
                 deleted AS (
                    DELETE FROM kaspa_pending_l2_submissions p
                    USING due d
                    WHERE p.l2_tx_hash = d.l2_tx_hash
                      AND EXISTS (
                        SELECT 1 FROM kaspa_l2_submissions f
                        WHERE f.l2_tx_hash = p.l2_tx_hash
                      )
                 )
                 SELECT l2_tx_hash, kaspa_txid, created_at FROM inserted",
                &[],
            )
            .await?;

        let entry_rows = tx
            .query(
                "WITH due AS (
                    SELECT kaspa_txid, recipient, amount_sompi
                    FROM kaspa_pending_entries
                    WHERE promote_after <= now()
                 ),
                 inserted AS (
                    INSERT INTO kaspa_entries (kaspa_txid, recipient, amount_sompi)
                    SELECT kaspa_txid, recipient, amount_sompi FROM due
                    ON CONFLICT DO NOTHING
                    RETURNING kaspa_txid, recipient, amount_sompi, created_at
                 ),
                 deleted AS (
                    DELETE FROM kaspa_pending_entries p
                    USING due d
                    WHERE p.kaspa_txid = d.kaspa_txid
                      AND EXISTS (
                        SELECT 1 FROM kaspa_entries f
                        WHERE f.kaspa_txid = p.kaspa_txid
                      )
                 )
                 SELECT kaspa_txid, recipient, amount_sompi, created_at FROM inserted",
                &[],
            )
            .await?;

        tx.commit().await?;

        let mut promoted = PromotedRows::default();
        for row in l2_rows {
            promoted.l2_submissions.push(FinalL2Submission {
                l2_tx_hash: vec_to_array(row.get(0)),
                kaspa_txid: vec_to_array(row.get(1)),
                created_at: row.get(2),
            });
        }
        for row in entry_rows {
            let amount_sompi: i64 = row.get(2);
            promoted.entries.push(FinalEntry {
                kaspa_txid: vec_to_array(row.get(0)),
                recipient: vec_to_array_20(row.get(1)),
                amount_sompi: u64::try_from(amount_sompi).context("negative amount_sompi")?,
                created_at: row.get(3),
            });
        }

        Ok(promoted)
    }
}

fn vec_to_array(bytes: Vec<u8>) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn vec_to_array_20(bytes: Vec<u8>) -> [u8; 20] {
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    out
}
