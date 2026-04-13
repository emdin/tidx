use std::sync::Arc;

use alloy::primitives::{Address, U256, keccak256};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use futures::future::join_all;
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState};
use crate::db::Pool;
use crate::sync::fetcher::RpcClient;

const ERC20_NAME: &str = "0x06fdde03";
const ERC20_SYMBOL: &str = "0x95d89b41";
const ERC20_DECIMALS: &str = "0x313ce567";
const ERC20_TOTAL_SUPPLY: &str = "0x18160ddd";
const ERC165_SUPPORTS_INTERFACE_SELECTOR: &str = "0x01ffc9a7";
const ERC165_INTERFACE_ID: [u8; 4] = [0x01, 0xff, 0xc9, 0xa7];
const ERC721_INTERFACE_ID: [u8; 4] = [0x80, 0xac, 0x58, 0xcd];
const ERC1155_INTERFACE_ID: [u8; 4] = [0xd9, 0xb6, 0x7a, 0x26];
const TRANSFER_TOPIC: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

#[derive(Deserialize)]
pub struct ChainQuery {
    #[serde(alias = "chain_id")]
    #[serde(rename = "chainId")]
    pub chain_id: Option<u64>,
}

#[derive(Deserialize)]
pub struct PaginationQuery {
    #[serde(alias = "chain_id")]
    #[serde(rename = "chainId")]
    pub chain_id: Option<u64>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ContractProfile {
    pub detected_kind: String,
    pub is_contract: bool,
    pub native_balance: String,
    pub bytecode_size: usize,
    pub code_hash: Option<String>,
    pub code_preview: Option<String>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub decimals: Option<u8>,
    pub total_supply: Option<String>,
    pub supports_erc165: bool,
    pub supports_erc721: bool,
    pub supports_erc1155: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractCreator {
    pub creator_address: String,
    pub tx_hash: String,
    pub block_num: i64,
    pub block_timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct AddressInspectResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub profile: ContractProfile,
    pub creator: Option<ContractCreator>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenHolding {
    pub token_address: String,
    pub balance: String,
    pub received_count: i64,
    pub sent_count: i64,
    pub last_block: i64,
    pub metadata: ContractProfile,
}

#[derive(Serialize)]
pub struct AddressPortfolioResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub native_balance: String,
    pub holdings: Vec<TokenHolding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenHolder {
    pub holder_address: String,
    pub balance: String,
}

#[derive(Serialize)]
pub struct TokenHoldersResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub profile: ContractProfile,
    pub total_holders: i64,
    pub holders: Vec<TokenHolder>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenTransfer {
    pub block_num: i64,
    pub block_timestamp: chrono::DateTime<chrono::Utc>,
    pub log_idx: i32,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount: String,
}

#[derive(Serialize)]
pub struct TokenTransfersResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub profile: ContractProfile,
    pub transfers: Vec<TokenTransfer>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractMethodRow {
    pub selector: String,
    pub call_count: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_block: i64,
}

#[derive(Serialize)]
pub struct ContractMethodsResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub methods: Vec<ContractMethodRow>,
}

pub async fn inspect_address(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
) -> Result<Json<AddressInspectResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let (_, rpc) = load_pool_and_rpc(&state, chain_id).await?;

    let creator = match state.get_pool(Some(chain_id)).await {
        Some(pool) => load_contract_creator(&pool, &normalized).await?,
        None => None,
    };
    let profile = inspect_contract_profile(rpc, &normalized).await;

    Ok(Json(AddressInspectResponse {
        ok: true,
        chain_id,
        address: normalized,
        profile,
        creator,
    }))
}

pub async fn address_portfolio(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<AddressPortfolioResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let (pool, rpc) = load_pool_and_rpc(&state, chain_id).await?;
    let limit = query.limit.unwrap_or(12).clamp(1, 25);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let padded = padded_address_topic(&normalized)?;
    let transfer_topic = hex::decode(TRANSFER_TOPIC)
        .map_err(|_| ApiError::Internal("Invalid transfer topic".to_string()))?;

    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;

    let rows = conn
        .query(
            "
            WITH balances AS (
                SELECT
                    address AS token_address,
                    (
                        SUM(CASE WHEN topic2 = $1 THEN abi_uint(data) ELSE 0::NUMERIC END)
                        - SUM(CASE WHEN topic1 = $1 THEN abi_uint(data) ELSE 0::NUMERIC END)
                    ) AS balance,
                    COUNT(*) FILTER (WHERE topic2 = $1) AS received_count,
                    COUNT(*) FILTER (WHERE topic1 = $1) AS sent_count,
                    MAX(block_num) AS last_block
                FROM logs
                WHERE topic0 = $2
                  AND (topic1 = $1 OR topic2 = $1)
                GROUP BY address
            )
            SELECT
                encode(token_address, 'hex') AS token_address,
                balance::TEXT AS balance,
                received_count,
                sent_count,
                last_block
            FROM balances
            WHERE balance > 0
            ORDER BY balance DESC, last_block DESC
            LIMIT $3
            OFFSET $4
            ",
            &[&padded, &transfer_topic, &limit, &offset],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load token balances: {e}")))?;

    let native_balance = rpc
        .get_balance(&normalized)
        .await
        .ok()
        .and_then(|hex| hex_quantity_to_decimal(&hex))
        .unwrap_or_else(|| "0".to_string());

    let base_holdings: Vec<_> = rows
        .iter()
        .map(|row| TokenHolding {
            token_address: format!("0x{}", row.get::<_, String>("token_address")),
            balance: row.get::<_, String>("balance"),
            received_count: row.get::<_, i64>("received_count"),
            sent_count: row.get::<_, i64>("sent_count"),
            last_block: row.get::<_, i64>("last_block"),
            metadata: ContractProfile::default(),
        })
        .collect();

    let profiles = join_all(
        base_holdings
            .iter()
            .map(|holding| inspect_token_profile(rpc.clone(), &holding.token_address)),
    )
    .await;

    let holdings = base_holdings
        .into_iter()
        .zip(profiles)
        .map(|(mut holding, metadata)| {
            holding.metadata = metadata;
            holding
        })
        .collect();

    Ok(Json(AddressPortfolioResponse {
        ok: true,
        chain_id,
        address: normalized,
        native_balance,
        holdings,
    }))
}

pub async fn token_holders(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<TokenHoldersResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let (pool, rpc) = load_pool_and_rpc(&state, chain_id).await?;
    let limit = query.limit.unwrap_or(10).clamp(1, 50);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let token_bytes = address_bytes(&normalized)?;
    let transfer_topic = hex::decode(TRANSFER_TOPIC)
        .map_err(|_| ApiError::Internal("Invalid transfer topic".to_string()))?;
    let zero_topic = padded_address_topic(ZERO_ADDRESS)?;

    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;

    let rows = conn
        .query(
            "
            WITH deltas AS (
                SELECT abi_address(topic1) AS holder, -abi_uint(data) AS delta
                FROM logs
                WHERE address = $1
                  AND topic0 = $2
                  AND topic1 IS NOT NULL
                  AND topic1 <> $3
                UNION ALL
                SELECT abi_address(topic2) AS holder, abi_uint(data) AS delta
                FROM logs
                WHERE address = $1
                  AND topic0 = $2
                  AND topic2 IS NOT NULL
                  AND topic2 <> $3
            ),
            balances AS (
                SELECT holder, SUM(delta) AS balance
                FROM deltas
                GROUP BY holder
                HAVING SUM(delta) > 0
            )
            SELECT
                encode(holder, 'hex') AS holder_address,
                balance::TEXT AS balance,
                COUNT(*) OVER() AS total_holders
            FROM balances
            ORDER BY balance DESC
            LIMIT $4
            OFFSET $5
            ",
            &[&token_bytes, &transfer_topic, &zero_topic, &limit, &offset],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load token holders: {e}")))?;

    let total_holders = rows
        .first()
        .map(|row| row.get::<_, i64>("total_holders"))
        .unwrap_or(0);
    let holders = rows
        .into_iter()
        .map(|row| TokenHolder {
            holder_address: format!("0x{}", row.get::<_, String>("holder_address")),
            balance: row.get::<_, String>("balance"),
        })
        .collect();

    Ok(Json(TokenHoldersResponse {
        ok: true,
        chain_id,
        address: normalized.clone(),
        profile: inspect_token_profile(rpc, &normalized).await,
        total_holders,
        holders,
    }))
}

pub async fn token_transfers(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<TokenTransfersResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let (pool, rpc) = load_pool_and_rpc(&state, chain_id).await?;
    let limit = query.limit.unwrap_or(15).clamp(1, 50);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let token_bytes = address_bytes(&normalized)?;
    let transfer_topic = hex::decode(TRANSFER_TOPIC)
        .map_err(|_| ApiError::Internal("Invalid transfer topic".to_string()))?;

    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;

    let rows = conn
        .query(
            "
            SELECT
                block_num,
                block_timestamp,
                log_idx,
                encode(tx_hash, 'hex') AS tx_hash,
                encode(abi_address(topic1), 'hex') AS from_address,
                encode(abi_address(topic2), 'hex') AS to_address,
                abi_uint(data)::TEXT AS amount
            FROM logs
            WHERE address = $1
              AND topic0 = $2
            ORDER BY block_num DESC, log_idx DESC
            LIMIT $3
            OFFSET $4
            ",
            &[&token_bytes, &transfer_topic, &limit, &offset],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load token transfers: {e}")))?;

    let transfers = rows
        .into_iter()
        .map(|row| TokenTransfer {
            block_num: row.get("block_num"),
            block_timestamp: row.get("block_timestamp"),
            log_idx: row.get("log_idx"),
            tx_hash: format!("0x{}", row.get::<_, String>("tx_hash")),
            from_address: format!("0x{}", row.get::<_, String>("from_address")),
            to_address: format!("0x{}", row.get::<_, String>("to_address")),
            amount: row.get("amount"),
        })
        .collect();

    Ok(Json(TokenTransfersResponse {
        ok: true,
        chain_id,
        address: normalized.clone(),
        profile: inspect_token_profile(rpc, &normalized).await,
        transfers,
    }))
}

pub async fn contract_methods(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<ContractMethodsResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let limit = query.limit.unwrap_or(12).clamp(1, 50);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let contract_bytes = address_bytes(&normalized)?;

    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;

    let rows = conn
        .query(
            "
            SELECT
                encode(substring(txs.input FROM 1 FOR 4), 'hex') AS selector,
                COUNT(*)::BIGINT AS call_count,
                COUNT(*) FILTER (WHERE receipts.status = 1)::BIGINT AS success_count,
                COUNT(*) FILTER (WHERE receipts.status = 0)::BIGINT AS failure_count,
                MAX(txs.block_num) AS last_block
            FROM txs
            LEFT JOIN receipts ON receipts.tx_hash = txs.hash
            WHERE txs.\"to\" = $1
              AND octet_length(txs.input) >= 4
            GROUP BY selector
            ORDER BY call_count DESC, last_block DESC
            LIMIT $2
            OFFSET $3
            ",
            &[&contract_bytes, &limit, &offset],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load contract methods: {e}")))?;

    let methods = rows
        .into_iter()
        .map(|row| ContractMethodRow {
            selector: format!("0x{}", row.get::<_, String>("selector")),
            call_count: row.get("call_count"),
            success_count: row.get("success_count"),
            failure_count: row.get("failure_count"),
            last_block: row.get("last_block"),
        })
        .collect();

    Ok(Json(ContractMethodsResponse {
        ok: true,
        chain_id,
        address: normalized,
        methods,
    }))
}

fn resolve_chain_id(state: &AppState, chain_id: Option<u64>) -> u64 {
    chain_id.unwrap_or(state.default_chain_id)
}

async fn load_pool_and_rpc(
    state: &AppState,
    chain_id: u64,
) -> Result<(Pool, Arc<RpcClient>), ApiError> {
    let pool = state
        .get_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let rpc = state.get_rpc(Some(chain_id)).await.ok_or_else(|| {
        ApiError::BadRequest(format!("RPC not configured for chain_id: {chain_id}"))
    })?;
    Ok((pool, rpc))
}

async fn load_contract_creator(
    pool: &Pool,
    address: &str,
) -> Result<Option<ContractCreator>, ApiError> {
    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let contract_bytes = address_bytes(address)?;
    let row = conn
        .query_opt(
            "
            SELECT
                encode(txs.hash, 'hex') AS tx_hash,
                encode(txs.\"from\", 'hex') AS creator_address,
                txs.block_num,
                txs.block_timestamp
            FROM receipts
            JOIN txs ON txs.hash = receipts.tx_hash
            WHERE receipts.contract_address = $1
            ORDER BY txs.block_num ASC, txs.idx ASC
            LIMIT 1
            ",
            &[&contract_bytes],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load contract creator: {e}")))?;

    Ok(row.map(|row| ContractCreator {
        creator_address: format!("0x{}", row.get::<_, String>("creator_address")),
        tx_hash: format!("0x{}", row.get::<_, String>("tx_hash")),
        block_num: row.get("block_num"),
        block_timestamp: row.get("block_timestamp"),
    }))
}

async fn inspect_token_profile(rpc: Arc<RpcClient>, address: &str) -> ContractProfile {
    let mut profile = inspect_contract_profile(rpc, address).await;
    if profile.detected_kind == "contract" && (profile.name.is_some() || profile.symbol.is_some()) {
        profile.detected_kind = "erc20".to_string();
    }
    profile
}

async fn inspect_contract_profile(rpc: Arc<RpcClient>, address: &str) -> ContractProfile {
    let native_balance = rpc
        .get_balance(address)
        .await
        .ok()
        .and_then(|hex| hex_quantity_to_decimal(&hex))
        .unwrap_or_else(|| "0".to_string());

    let code_hex = rpc
        .get_code(address)
        .await
        .unwrap_or_else(|_| "0x".to_string());
    let code_bytes = decode_hex_data(&code_hex).unwrap_or_default();
    if code_bytes.is_empty() {
        return ContractProfile {
            detected_kind: "account".to_string(),
            is_contract: false,
            native_balance,
            ..ContractProfile::default()
        };
    }

    let code_hash = Some(format!("0x{}", hex::encode(keccak256(&code_bytes))));
    let code_preview = Some(preview_hex(&code_bytes, 32));

    let name_fut = rpc_call_string(rpc.clone(), address, ERC20_NAME);
    let symbol_fut = rpc_call_string(rpc.clone(), address, ERC20_SYMBOL);
    let decimals_fut = rpc_call_u8(rpc.clone(), address, ERC20_DECIMALS);
    let total_supply_fut = rpc_call_uint(rpc.clone(), address, ERC20_TOTAL_SUPPLY);
    let erc165_call = supports_interface_call(ERC165_INTERFACE_ID);
    let erc721_call = supports_interface_call(ERC721_INTERFACE_ID);
    let erc1155_call = supports_interface_call(ERC1155_INTERFACE_ID);
    let erc165_fut = rpc_call_bool(rpc.clone(), address, &erc165_call);
    let erc721_fut = rpc_call_bool(rpc.clone(), address, &erc721_call);
    let erc1155_fut = rpc_call_bool(rpc, address, &erc1155_call);

    let (name, symbol, decimals, total_supply, supports_erc165, supports_erc721, supports_erc1155) = tokio::join!(
        name_fut,
        symbol_fut,
        decimals_fut,
        total_supply_fut,
        erc165_fut,
        erc721_fut,
        erc1155_fut
    );

    let detected_kind = if supports_erc721 == Some(true) {
        "erc721"
    } else if supports_erc1155 == Some(true) {
        "erc1155"
    } else if decimals.is_some() || total_supply.is_some() || name.is_some() || symbol.is_some() {
        "erc20"
    } else {
        "contract"
    }
    .to_string();

    ContractProfile {
        detected_kind,
        is_contract: true,
        native_balance,
        bytecode_size: code_bytes.len(),
        code_hash,
        code_preview,
        name,
        symbol,
        decimals,
        total_supply,
        supports_erc165: supports_erc165.unwrap_or(false),
        supports_erc721: supports_erc721.unwrap_or(false),
        supports_erc1155: supports_erc1155.unwrap_or(false),
    }
}

async fn rpc_call_string(rpc: Arc<RpcClient>, address: &str, data: &str) -> Option<String> {
    let raw = rpc.eth_call(address, data).await.ok()?;
    decode_string_output(&raw)
}

async fn rpc_call_u8(rpc: Arc<RpcClient>, address: &str, data: &str) -> Option<u8> {
    let raw = rpc.eth_call(address, data).await.ok()?;
    let value = decode_uint_output(&raw)?;
    value.parse().ok()
}

async fn rpc_call_uint(rpc: Arc<RpcClient>, address: &str, data: &str) -> Option<String> {
    let raw = rpc.eth_call(address, data).await.ok()?;
    decode_uint_output(&raw)
}

async fn rpc_call_bool(rpc: Arc<RpcClient>, address: &str, data: &str) -> Option<bool> {
    let raw = rpc.eth_call(address, data).await.ok()?;
    decode_bool_output(&raw)
}

fn normalize_address(value: &str) -> Result<String, ApiError> {
    let candidate = value.trim();
    let with_prefix = if candidate.starts_with("0x") {
        candidate.to_ascii_lowercase()
    } else {
        format!("0x{}", candidate.to_ascii_lowercase())
    };
    with_prefix
        .parse::<Address>()
        .map(|address| address.to_string().to_ascii_lowercase())
        .map_err(|_| ApiError::BadRequest("Invalid address.".to_string()))
}

fn address_bytes(address: &str) -> Result<Vec<u8>, ApiError> {
    hex::decode(address.trim_start_matches("0x"))
        .map_err(|_| ApiError::BadRequest("Invalid address.".to_string()))
}

fn padded_address_topic(address: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = address_bytes(address)?;
    let mut padded = vec![0u8; 12];
    padded.extend(bytes);
    Ok(padded)
}

fn decode_hex_data(value: &str) -> Option<Vec<u8>> {
    let normalized = value.trim();
    let body = normalized.strip_prefix("0x").unwrap_or(normalized);
    if body.is_empty() {
        return Some(Vec::new());
    }
    hex::decode(body).ok()
}

fn hex_quantity_to_decimal(value: &str) -> Option<String> {
    let normalized = value.trim_start_matches("0x");
    if normalized.is_empty() {
        return Some("0".to_string());
    }
    U256::from_str_radix(normalized, 16)
        .ok()
        .map(|value| value.to_string())
}

fn decode_uint_output(value: &str) -> Option<String> {
    let bytes = decode_hex_data(value)?;
    if bytes.len() < 32 {
        return None;
    }
    Some(U256::from_be_slice(&bytes[bytes.len() - 32..]).to_string())
}

fn decode_bool_output(value: &str) -> Option<bool> {
    let bytes = decode_hex_data(value)?;
    bytes.last().copied().map(|byte| byte != 0)
}

fn decode_string_output(value: &str) -> Option<String> {
    let bytes = decode_hex_data(value)?;
    if bytes.is_empty() {
        return None;
    }

    if bytes.len() == 32 {
        return decode_bytes32_string(&bytes);
    }

    if bytes.len() < 64 {
        return None;
    }

    let offset = u256_word_to_usize(&bytes[0..32])?;
    if offset + 32 > bytes.len() {
        return decode_bytes32_string(&bytes[0..32]);
    }

    let len = u256_word_to_usize(&bytes[offset..offset + 32])?;
    let start = offset + 32;
    let end = start + len;
    if end > bytes.len() {
        return None;
    }

    let text = String::from_utf8_lossy(&bytes[start..end])
        .trim()
        .to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn decode_bytes32_string(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn u256_word_to_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != 32 {
        return None;
    }
    if bytes[..24].iter().any(|byte| *byte != 0) {
        return None;
    }
    let raw = u64::from_be_bytes(bytes[24..32].try_into().ok()?);
    usize::try_from(raw).ok()
}

fn supports_interface_call(interface_id: [u8; 4]) -> String {
    format!(
        "{}{}{}",
        ERC165_SUPPORTS_INTERFACE_SELECTOR,
        "0".repeat(56),
        hex::encode(interface_id)
    )
}

fn preview_hex(bytes: &[u8], max_bytes: usize) -> String {
    let preview = &bytes[..bytes.len().min(max_bytes)];
    let suffix = if bytes.len() > max_bytes { "…" } else { "" };
    format!("0x{}{}", hex::encode(preview), suffix)
}
