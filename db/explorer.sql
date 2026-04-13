CREATE TABLE IF NOT EXISTS address_labels (
    address         BYTEA PRIMARY KEY,
    label           TEXT NOT NULL,
    category        TEXT,
    website         TEXT,
    notes           TEXT,
    is_official     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_address_labels_label ON address_labels (lower(label));
CREATE INDEX IF NOT EXISTS idx_address_labels_category ON address_labels (lower(category));

CREATE TABLE IF NOT EXISTS contract_verifications (
    address                 BYTEA PRIMARY KEY,
    contract_name           TEXT NOT NULL,
    language                TEXT,
    compiler_version        TEXT,
    optimization_enabled    BOOLEAN,
    optimization_runs       INT4,
    license                 TEXT,
    constructor_args        TEXT,
    abi                     JSONB NOT NULL,
    source_code             TEXT,
    metadata                JSONB,
    verified_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_contract_verifications_name ON contract_verifications (lower(contract_name));

CREATE TABLE IF NOT EXISTS token_metadata (
    address             BYTEA PRIMARY KEY,
    detected_kind       TEXT NOT NULL,
    name                TEXT,
    symbol              TEXT,
    decimals            INT4,
    total_supply        TEXT,
    bytecode_size       INT4,
    code_hash           TEXT,
    supports_erc165     BOOLEAN NOT NULL DEFAULT FALSE,
    supports_erc721     BOOLEAN NOT NULL DEFAULT FALSE,
    supports_erc1155    BOOLEAN NOT NULL DEFAULT FALSE,
    source              TEXT NOT NULL DEFAULT 'rpc',
    refreshed_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_token_metadata_name ON token_metadata (lower(name));
CREATE INDEX IF NOT EXISTS idx_token_metadata_symbol ON token_metadata (lower(symbol));
CREATE INDEX IF NOT EXISTS idx_token_metadata_kind ON token_metadata (detected_kind);
