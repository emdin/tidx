CREATE TABLE IF NOT EXISTS kaspa_l2_submissions (
    l2_tx_hash  String,
    kaspa_txid  String,
    created_at  DateTime64(3, 'UTC')
) ENGINE = ReplacingMergeTree()
ORDER BY (l2_tx_hash)
