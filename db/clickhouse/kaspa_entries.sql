CREATE TABLE IF NOT EXISTS kaspa_entries (
    kaspa_txid    String,
    recipient     String,
    amount_sompi  UInt64,
    created_at    DateTime64(3, 'UTC')
) ENGINE = ReplacingMergeTree()
ORDER BY (recipient, kaspa_txid)
