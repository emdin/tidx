use std::{net::SocketAddr, str::FromStr, sync::Arc};

use alloy::{
    dyn_abi::{DynSolValue, FunctionExt, JsonAbiExt, Specifier},
    json_abi::{Function, JsonAbi, StateMutability},
    primitives::{Address, U256, keccak256},
};
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

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
const APPROVAL_TOPIC: &str = "8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925";
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

#[derive(Debug, Clone, Serialize)]
pub struct AddressLabel {
    pub label: String,
    pub category: Option<String>,
    pub website: Option<String>,
    pub notes: Option<String>,
    pub is_official: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationSummary {
    pub contract_name: String,
    pub language: Option<String>,
    pub compiler_version: Option<String>,
    pub optimization_enabled: Option<bool>,
    pub optimization_runs: Option<i32>,
    pub license: Option<String>,
    pub constructor_args: Option<String>,
    pub verified_at: chrono::DateTime<chrono::Utc>,
    pub has_source_code: bool,
    pub abi_function_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractVerificationDetail {
    pub summary: VerificationSummary,
    pub abi: Value,
    pub source_code: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractFunctionArg {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadFunctionInfo {
    pub name: String,
    pub signature: String,
    pub selector: String,
    pub state_mutability: String,
    pub inputs: Vec<ContractFunctionArg>,
    pub outputs: Vec<ContractFunctionArg>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadContractValue {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub entity_type: String,
    pub address: Option<String>,
    pub block_num: Option<i64>,
    pub tx_hash: Option<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub href: String,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(alias = "chain_id")]
    #[serde(rename = "chainId")]
    pub chain_id: Option<u64>,
    pub q: String,
}

#[derive(Deserialize)]
pub struct LabelUpsertRequest {
    pub label: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub is_official: bool,
}

#[derive(Deserialize)]
pub struct VerifyContractRequest {
    pub contract_name: String,
    pub abi: Value,
    #[serde(default)]
    pub source_code: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub compiler_version: Option<String>,
    #[serde(default)]
    pub optimization_enabled: Option<bool>,
    #[serde(default)]
    pub optimization_runs: Option<i32>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub constructor_args: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Deserialize)]
pub struct ReadContractRequest {
    pub selector: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Serialize)]
pub struct AddressInspectResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub profile: ContractProfile,
    pub creator: Option<ContractCreator>,
    pub label: Option<AddressLabel>,
    pub verification: Option<VerificationSummary>,
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
pub struct TokenApproval {
    pub block_num: i64,
    pub block_timestamp: chrono::DateTime<chrono::Utc>,
    pub log_idx: i32,
    pub tx_hash: String,
    pub owner_address: String,
    pub spender_address: String,
    pub amount: String,
}

#[derive(Serialize)]
pub struct TokenApprovalsResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub profile: ContractProfile,
    pub approvals: Vec<TokenApproval>,
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
pub struct TokenListItem {
    pub address: String,
    pub detected_kind: String,
    pub label: Option<String>,
    pub is_official: bool,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub decimals: Option<i32>,
    pub total_supply: Option<String>,
    pub transfer_count: i64,
    pub approval_count: i64,
    pub last_seen_block: i64,
}

#[derive(Serialize)]
pub struct TokensResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub page: i64,
    pub limit: i64,
    pub tokens: Vec<TokenListItem>,
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

#[derive(Serialize)]
pub struct SearchResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub query: String,
    pub results: Vec<SearchHit>,
}

#[derive(Serialize)]
pub struct ContractVerificationResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub label: Option<AddressLabel>,
    pub verification: Option<ContractVerificationDetail>,
    pub read_functions: Vec<ReadFunctionInfo>,
}

#[derive(Serialize)]
pub struct ReadContractResponse {
    pub ok: bool,
    pub chain_id: u64,
    pub address: String,
    pub function: ReadFunctionInfo,
    pub outputs: Vec<ReadContractValue>,
}

#[derive(Serialize)]
pub struct AdminCapabilitiesResponse {
    pub ok: bool,
    pub can_write_metadata: bool,
}

pub async fn admin_capabilities(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<AdminCapabilitiesResponse>, ApiError> {
    Ok(Json(AdminCapabilitiesResponse {
        ok: true,
        can_write_metadata: state.is_trusted_ip(&addr),
    }))
}

pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ApiError> {
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let trimmed = query.q.trim().to_string();

    if trimmed.is_empty() {
        return Ok(Json(SearchResponse {
            ok: true,
            chain_id,
            query: trimmed,
            results: Vec::new(),
        }));
    }

    let normalized = normalize_search_input(&trimmed);
    if let Some(hit) = direct_search_hit(&normalized) {
        return Ok(Json(SearchResponse {
            ok: true,
            chain_id,
            query: trimmed,
            results: vec![hit],
        }));
    }

    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let like = format!("%{}%", trimmed.to_lowercase());
    let rows = conn
        .query(
            "
            SELECT
                encode(addr, 'hex') AS address,
                label,
                subtitle,
                entity_type,
                rank,
                official_rank,
                entity_rank
            FROM (
                SELECT
                    address AS addr,
                    label,
                    COALESCE(category, '') AS subtitle,
                    'label' AS entity_type,
                    CASE
                        WHEN lower(label) = $1 THEN 0
                        WHEN lower(COALESCE(category, '')) = $1 THEN 1
                        ELSE 2
                    END AS rank,
                    CASE WHEN is_official THEN 0 ELSE 1 END AS official_rank,
                    0 AS entity_rank
                FROM address_labels
                WHERE lower(label) LIKE $2
                   OR lower(COALESCE(category, '')) LIKE $2
                UNION ALL
                SELECT
                    address AS addr,
                    contract_name AS label,
                    COALESCE(compiler_version, '') AS subtitle,
                    'verified_contract' AS entity_type,
                    CASE WHEN lower(contract_name) = $1 THEN 0 ELSE 2 END AS rank,
                    0 AS official_rank,
                    1 AS entity_rank
                FROM contract_verifications
                WHERE lower(contract_name) LIKE $2
                UNION ALL
                SELECT
                    tm.address AS addr,
                    COALESCE(al.label, tm.symbol, tm.name, encode(tm.address, 'hex')) AS label,
                    COALESCE(tm.name, tm.detected_kind) AS subtitle,
                    'token' AS entity_type,
                    CASE
                        WHEN lower(COALESCE(tm.symbol, '')) = $1 THEN 0
                        WHEN lower(COALESCE(tm.name, '')) = $1 THEN 1
                        WHEN lower(COALESCE(al.label, '')) = $1 THEN 1
                        ELSE 3
                    END AS rank,
                    CASE WHEN COALESCE(al.is_official, FALSE) THEN 0 ELSE 1 END AS official_rank,
                    2 AS entity_rank
                FROM token_metadata tm
                LEFT JOIN address_labels al ON al.address = tm.address
                WHERE lower(COALESCE(tm.symbol, '')) LIKE $2
                   OR lower(COALESCE(tm.name, '')) LIKE $2
                   OR lower(COALESCE(al.label, '')) LIKE $2
            ) search_hits
            ORDER BY rank ASC, official_rank ASC, entity_rank ASC, label ASC
            LIMIT 20
            ",
            &[&trimmed.to_lowercase(), &like],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to search explorer metadata: {e}")))?;

    let results = rows
        .into_iter()
        .map(|row| {
            let address = format!("0x{}", row.get::<_, String>("address"));
            let entity_type = row.get::<_, String>("entity_type");
            SearchHit {
                entity_type: entity_type.clone(),
                address: Some(address.clone()),
                block_num: None,
                tx_hash: None,
                title: row.get("label"),
                subtitle: optional_non_empty(row.get::<_, String>("subtitle")),
                href: if entity_type == "token" {
                    format!("/explore/token/{address}")
                } else {
                    format!("/explore/address/{address}")
                },
            }
        })
        .collect();

    Ok(Json(SearchResponse {
        ok: true,
        chain_id,
        query: trimmed,
        results,
    }))
}

pub async fn tokens(
    State(state): State<AppState>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<TokensResponse>, ApiError> {
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let limit = query.limit.unwrap_or(25).clamp(1, 50);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let transfer_topic = hex::decode(TRANSFER_TOPIC)
        .map_err(|_| ApiError::Internal("Invalid transfer topic".to_string()))?;
    let approval_topic = hex::decode(APPROVAL_TOPIC)
        .map_err(|_| ApiError::Internal("Invalid approval topic".to_string()))?;
    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;

    let rows = conn
        .query(
            "
            WITH activity AS (
                SELECT
                    address,
                    COUNT(*) FILTER (WHERE topic0 = $1)::BIGINT AS transfer_count,
                    COUNT(*) FILTER (WHERE topic0 = $2)::BIGINT AS approval_count,
                    MAX(block_num) AS last_seen_block
                FROM logs
                WHERE topic0 = $1 OR topic0 = $2
                GROUP BY address
            )
            SELECT
                encode(activity.address, 'hex') AS address,
                COALESCE(tm.detected_kind, 'token') AS detected_kind,
                al.label,
                COALESCE(al.is_official, FALSE) AS is_official,
                tm.name,
                tm.symbol,
                tm.decimals,
                tm.total_supply,
                activity.transfer_count,
                activity.approval_count,
                activity.last_seen_block
            FROM activity
            LEFT JOIN token_metadata tm ON tm.address = activity.address
            LEFT JOIN address_labels al ON al.address = activity.address
            ORDER BY
                CASE WHEN COALESCE(al.is_official, FALSE) THEN 0 ELSE 1 END,
                CASE WHEN COALESCE(tm.symbol, '') <> '' THEN 0 ELSE 1 END,
                CASE WHEN COALESCE(tm.name, '') <> '' THEN 0 ELSE 1 END,
                activity.transfer_count DESC,
                activity.approval_count DESC,
                activity.last_seen_block DESC
            LIMIT $3
            OFFSET $4
            ",
            &[&transfer_topic, &approval_topic, &limit, &offset],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load token list: {e}")))?;

    let tokens = rows
        .into_iter()
        .map(|row| TokenListItem {
            address: format!("0x{}", row.get::<_, String>("address")),
            detected_kind: row.get("detected_kind"),
            label: row.get("label"),
            is_official: row.get("is_official"),
            name: row.get("name"),
            symbol: row.get("symbol"),
            decimals: row.get("decimals"),
            total_supply: row.get("total_supply"),
            transfer_count: row.get("transfer_count"),
            approval_count: row.get("approval_count"),
            last_seen_block: row.get("last_seen_block"),
        })
        .collect();

    Ok(Json(TokensResponse {
        ok: true,
        chain_id,
        page,
        limit,
        tokens,
    }))
}

pub async fn contract_verification(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
) -> Result<Json<ContractVerificationResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let label = load_address_label(&pool, &normalized).await?;
    let verification = load_contract_verification(&pool, &normalized).await?;
    let read_functions = verification
        .as_ref()
        .and_then(|record| parse_json_abi(&record.abi).ok())
        .map(read_functions_from_abi)
        .unwrap_or_default();

    Ok(Json(ContractVerificationResponse {
        ok: true,
        chain_id,
        address: normalized,
        label,
        verification,
        read_functions,
    }))
}

pub async fn verify_contract(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
    Json(payload): Json<VerifyContractRequest>,
) -> Result<Json<ContractVerificationResponse>, ApiError> {
    if !state.is_trusted_ip(&addr) {
        return Err(ApiError::Forbidden(
            "Contract verification writes are only allowed from trusted IPs".to_string(),
        ));
    }

    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_write_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    validate_contract_verification_payload(&payload)?;
    save_contract_verification(&pool, &normalized, &payload).await?;

    contract_verification(
        State(state),
        Path(normalized),
        Query(ChainQuery {
            chain_id: Some(chain_id),
        }),
    )
    .await
}

pub async fn upsert_label(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
    Json(payload): Json<LabelUpsertRequest>,
) -> Result<Json<AddressInspectResponse>, ApiError> {
    if !state.is_trusted_ip(&addr) {
        return Err(ApiError::Forbidden(
            "Label writes are only allowed from trusted IPs".to_string(),
        ));
    }

    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_write_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    save_address_label(&pool, &normalized, &payload).await?;

    inspect_address(
        State(state),
        Path(normalized),
        Query(ChainQuery {
            chain_id: Some(chain_id),
        }),
    )
    .await
}

pub async fn refresh_address_metadata(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
) -> Result<Json<AddressInspectResponse>, ApiError> {
    if !state.is_trusted_ip(&addr) {
        return Err(ApiError::Forbidden(
            "Metadata refresh is only allowed from trusted IPs".to_string(),
        ));
    }

    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let write_pool = state
        .get_write_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let rpc = state.get_rpc(Some(chain_id)).await.ok_or_else(|| {
        ApiError::BadRequest(format!("RPC not configured for chain_id: {chain_id}"))
    })?;
    let profile = inspect_contract_profile(rpc, &normalized).await;
    let _ = upsert_token_metadata(&write_pool, &normalized, &profile).await;

    inspect_address(
        State(state),
        Path(normalized),
        Query(ChainQuery {
            chain_id: Some(chain_id),
        }),
    )
    .await
}

pub async fn read_contract(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
    Json(payload): Json<ReadContractRequest>,
) -> Result<Json<ReadContractResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let pool = state
        .get_pool(Some(chain_id))
        .await
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown chain_id: {chain_id}")))?;
    let rpc = state.get_rpc(Some(chain_id)).await.ok_or_else(|| {
        ApiError::BadRequest(format!("RPC not configured for chain_id: {chain_id}"))
    })?;
    let verification = load_contract_verification(&pool, &normalized)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound("No stored contract verification exists for this address".to_string())
        })?;
    let abi = parse_json_abi(&verification.abi)
        .map_err(|e| ApiError::Internal(format!("Invalid stored ABI: {e}")))?;
    let function = find_read_function_by_selector(&abi, &payload.selector)
        .ok_or_else(|| ApiError::BadRequest("Function selector not found in stored ABI".to_string()))?;
    let values = coerce_read_args(&function, &payload.args)
        .map_err(|e| ApiError::BadRequest(format!("Invalid function arguments: {e}")))?;
    let call_data = function
        .abi_encode_input(&values)
        .map_err(|e| ApiError::BadRequest(format!("Failed to ABI-encode call: {e}")))?;
    let raw = rpc
        .eth_call(&normalized, &format!("0x{}", hex::encode(call_data)))
        .await
        .map_err(|e| ApiError::QueryError(format!("eth_call failed: {e}")))?;
    let output_bytes = decode_hex_data(&raw)
        .ok_or_else(|| ApiError::QueryError("Invalid hex data returned from eth_call".to_string()))?;
    let outputs = function
        .abi_decode_output(&output_bytes)
        .map_err(|e| ApiError::QueryError(format!("Failed to decode contract output: {e}")))?;

    Ok(Json(ReadContractResponse {
        ok: true,
        chain_id,
        address: normalized,
        function: read_function_info(&function),
        outputs: outputs
            .into_iter()
            .map(|value| ReadContractValue {
                kind: value
                    .sol_type_name()
                    .unwrap_or_else(|| "unknown".into())
                    .to_string(),
                value: dyn_value_to_string(&value),
            })
            .collect(),
    }))
}

pub async fn inspect_address(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<ChainQuery>,
) -> Result<Json<AddressInspectResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let (pool, rpc) = load_pool_and_rpc(&state, chain_id).await?;
    let creator = load_contract_creator(&pool, &normalized).await?;
    let label = load_address_label(&pool, &normalized).await?;
    let verification = load_contract_verification(&pool, &normalized)
        .await?
        .map(|detail| detail.summary);
    let profile = inspect_contract_profile(rpc, &normalized).await;

    if profile.is_contract {
        if let Some(write_pool) = state.get_write_pool(Some(chain_id)).await {
            if let Err(error) = upsert_token_metadata(&write_pool, &normalized, &profile).await {
                warn!(address = %normalized, error = %error, "Failed to refresh token metadata cache");
            }
        }
    }

    Ok(Json(AddressInspectResponse {
        ok: true,
        chain_id,
        address: normalized,
        profile,
        creator,
        label,
        verification,
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
                        SUM(
                            CASE
                                WHEN topic2 = $1 AND octet_length(data) >= 32 THEN abi_uint(data)
                                WHEN topic2 = $1 AND topic3 IS NOT NULL THEN abi_uint(topic3)
                                ELSE 0::NUMERIC
                            END
                        )
                        - SUM(
                            CASE
                                WHEN topic1 = $1 AND octet_length(data) >= 32 THEN abi_uint(data)
                                WHEN topic1 = $1 AND topic3 IS NOT NULL THEN abi_uint(topic3)
                                ELSE 0::NUMERIC
                            END
                        )
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
                SELECT
                    abi_address(topic1) AS holder,
                    -CASE
                        WHEN octet_length(data) >= 32 THEN abi_uint(data)
                        WHEN topic3 IS NOT NULL THEN abi_uint(topic3)
                        ELSE 0::NUMERIC
                    END AS delta
                FROM logs
                WHERE address = $1
                  AND topic0 = $2
                  AND topic1 IS NOT NULL
                  AND topic1 <> $3
                UNION ALL
                SELECT
                    abi_address(topic2) AS holder,
                    CASE
                        WHEN octet_length(data) >= 32 THEN abi_uint(data)
                        WHEN topic3 IS NOT NULL THEN abi_uint(topic3)
                        ELSE 0::NUMERIC
                    END AS delta
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
                CASE
                    WHEN octet_length(data) >= 32 THEN abi_uint(data)::TEXT
                    WHEN topic3 IS NOT NULL THEN abi_uint(topic3)::TEXT
                    ELSE '0'
                END AS amount
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

pub async fn token_approvals(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<TokenApprovalsResponse>, ApiError> {
    let normalized = normalize_address(&address)?;
    let chain_id = resolve_chain_id(&state, query.chain_id);
    let (pool, rpc) = load_pool_and_rpc(&state, chain_id).await?;
    let limit = query.limit.unwrap_or(12).clamp(1, 50);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let token_bytes = address_bytes(&normalized)?;
    let approval_topic = hex::decode(APPROVAL_TOPIC)
        .map_err(|_| ApiError::Internal("Invalid approval topic".to_string()))?;

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
                encode(abi_address(topic1), 'hex') AS owner_address,
                encode(abi_address(topic2), 'hex') AS spender_address,
                CASE
                    WHEN octet_length(data) >= 32 THEN abi_uint(data)::TEXT
                    WHEN topic3 IS NOT NULL THEN abi_uint(topic3)::TEXT
                    ELSE '0'
                END AS amount
            FROM logs
            WHERE address = $1
              AND topic0 = $2
            ORDER BY block_num DESC, log_idx DESC
            LIMIT $3
            OFFSET $4
            ",
            &[&token_bytes, &approval_topic, &limit, &offset],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load token approvals: {e}")))?;

    let approvals = rows
        .into_iter()
        .map(|row| TokenApproval {
            block_num: row.get("block_num"),
            block_timestamp: row.get("block_timestamp"),
            log_idx: row.get("log_idx"),
            tx_hash: format!("0x{}", row.get::<_, String>("tx_hash")),
            owner_address: format!("0x{}", row.get::<_, String>("owner_address")),
            spender_address: format!("0x{}", row.get::<_, String>("spender_address")),
            amount: row.get("amount"),
        })
        .collect();

    Ok(Json(TokenApprovalsResponse {
        ok: true,
        chain_id,
        address: normalized.clone(),
        profile: inspect_token_profile(rpc, &normalized).await,
        approvals,
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

async fn load_address_label(pool: &Pool, address: &str) -> Result<Option<AddressLabel>, ApiError> {
    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let address_bytes = address_bytes(address)?;
    let row = conn
        .query_opt(
            "
            SELECT label, category, website, notes, is_official
            FROM address_labels
            WHERE address = $1
            LIMIT 1
            ",
            &[&address_bytes],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load address label: {e}")))?;

    Ok(row.map(|row| AddressLabel {
        label: row.get("label"),
        category: row.get("category"),
        website: row.get("website"),
        notes: row.get("notes"),
        is_official: row.get("is_official"),
    }))
}

async fn save_address_label(
    pool: &Pool,
    address: &str,
    payload: &LabelUpsertRequest,
) -> Result<(), ApiError> {
    if payload.label.trim().is_empty() {
        return Err(ApiError::BadRequest("Label cannot be empty".to_string()));
    }

    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let address_bytes = address_bytes(address)?;
    conn.execute(
        "
        INSERT INTO address_labels (address, label, category, website, notes, is_official, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, now())
        ON CONFLICT (address) DO UPDATE SET
            label = EXCLUDED.label,
            category = EXCLUDED.category,
            website = EXCLUDED.website,
            notes = EXCLUDED.notes,
            is_official = EXCLUDED.is_official,
            updated_at = now()
        ",
        &[
            &address_bytes,
            &payload.label.trim(),
            &payload.category,
            &payload.website,
            &payload.notes,
            &payload.is_official,
        ],
    )
    .await
    .map_err(|e| ApiError::QueryError(format!("Failed to upsert label: {e}")))?;
    Ok(())
}

async fn load_contract_verification(
    pool: &Pool,
    address: &str,
) -> Result<Option<ContractVerificationDetail>, ApiError> {
    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let address_bytes = address_bytes(address)?;
    let row = conn
        .query_opt(
            "
            SELECT
                contract_name,
                language,
                compiler_version,
                optimization_enabled,
                optimization_runs,
                license,
                constructor_args,
                abi,
                source_code,
                metadata,
                verified_at
            FROM contract_verifications
            WHERE address = $1
            LIMIT 1
            ",
            &[&address_bytes],
        )
        .await
        .map_err(|e| ApiError::QueryError(format!("Failed to load contract verification: {e}")))?;

    Ok(row.map(|row| {
        let abi: Value = row.get("abi");
        let source_code: Option<String> = row.get("source_code");
        let metadata: Option<Value> = row.get("metadata");
        let contract_name: String = row.get("contract_name");
        ContractVerificationDetail {
            summary: VerificationSummary {
                contract_name,
                language: row.get("language"),
                compiler_version: row.get("compiler_version"),
                optimization_enabled: row.get("optimization_enabled"),
                optimization_runs: row.get("optimization_runs"),
                license: row.get("license"),
                constructor_args: row.get("constructor_args"),
                verified_at: row.get("verified_at"),
                has_source_code: source_code
                    .as_ref()
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false),
                abi_function_count: parse_json_abi(&abi)
                    .map(|json_abi| json_abi.functions.values().map(Vec::len).sum())
                    .unwrap_or(0),
            },
            abi,
            source_code,
            metadata,
        }
    }))
}

async fn save_contract_verification(
    pool: &Pool,
    address: &str,
    payload: &VerifyContractRequest,
) -> Result<(), ApiError> {
    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let address_bytes = address_bytes(address)?;

    conn.execute(
        "
        INSERT INTO contract_verifications (
            address,
            contract_name,
            language,
            compiler_version,
            optimization_enabled,
            optimization_runs,
            license,
            constructor_args,
            abi,
            source_code,
            metadata,
            verified_at,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now(), now())
        ON CONFLICT (address) DO UPDATE SET
            contract_name = EXCLUDED.contract_name,
            language = EXCLUDED.language,
            compiler_version = EXCLUDED.compiler_version,
            optimization_enabled = EXCLUDED.optimization_enabled,
            optimization_runs = EXCLUDED.optimization_runs,
            license = EXCLUDED.license,
            constructor_args = EXCLUDED.constructor_args,
            abi = EXCLUDED.abi,
            source_code = EXCLUDED.source_code,
            metadata = EXCLUDED.metadata,
            updated_at = now()
        ",
        &[
            &address_bytes,
            &payload.contract_name.trim(),
            &payload.language,
            &payload.compiler_version,
            &payload.optimization_enabled,
            &payload.optimization_runs,
            &payload.license,
            &payload.constructor_args,
            &payload.abi,
            &payload.source_code,
            &payload.metadata,
        ],
    )
    .await
    .map_err(|e| ApiError::QueryError(format!("Failed to save contract verification: {e}")))?;

    Ok(())
}

async fn upsert_token_metadata(
    pool: &Pool,
    address: &str,
    profile: &ContractProfile,
) -> Result<(), ApiError> {
    let conn = pool
        .get()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get DB connection: {e}")))?;
    let address_bytes = address_bytes(address)?;
    let bytecode_size = i32::try_from(profile.bytecode_size)
        .map_err(|_| ApiError::Internal("Bytecode size overflow".to_string()))?;
    let decimals = profile.decimals.map(i32::from);

    conn.execute(
        "
        INSERT INTO token_metadata (
            address,
            detected_kind,
            name,
            symbol,
            decimals,
            total_supply,
            bytecode_size,
            code_hash,
            supports_erc165,
            supports_erc721,
            supports_erc1155,
            source,
            refreshed_at,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'rpc', now(), now())
        ON CONFLICT (address) DO UPDATE SET
            detected_kind = EXCLUDED.detected_kind,
            name = EXCLUDED.name,
            symbol = EXCLUDED.symbol,
            decimals = EXCLUDED.decimals,
            total_supply = EXCLUDED.total_supply,
            bytecode_size = EXCLUDED.bytecode_size,
            code_hash = EXCLUDED.code_hash,
            supports_erc165 = EXCLUDED.supports_erc165,
            supports_erc721 = EXCLUDED.supports_erc721,
            supports_erc1155 = EXCLUDED.supports_erc1155,
            source = EXCLUDED.source,
            refreshed_at = now(),
            updated_at = now()
        ",
        &[
            &address_bytes,
            &profile.detected_kind,
            &profile.name,
            &profile.symbol,
            &decimals,
            &profile.total_supply,
            &bytecode_size,
            &profile.code_hash,
            &profile.supports_erc165,
            &profile.supports_erc721,
            &profile.supports_erc1155,
        ],
    )
    .await
    .map_err(|e| ApiError::QueryError(format!("Failed to upsert token metadata: {e}")))?;

    Ok(())
}

fn validate_contract_verification_payload(payload: &VerifyContractRequest) -> Result<(), ApiError> {
    if payload.contract_name.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "contract_name cannot be empty".to_string(),
        ));
    }

    parse_json_abi(&payload.abi)
        .map(|_| ())
        .map_err(|e| ApiError::BadRequest(format!("Invalid ABI JSON: {e}")))
}

fn parse_json_abi(value: &Value) -> Result<JsonAbi, serde_json::Error> {
    serde_json::from_value(value.clone())
}

fn read_functions_from_abi(abi: JsonAbi) -> Vec<ReadFunctionInfo> {
    abi.functions
        .into_values()
        .flatten()
        .filter(|function| {
            matches!(
                function.state_mutability,
                StateMutability::View | StateMutability::Pure
            )
        })
        .map(|function| read_function_info(&function))
        .collect()
}

fn read_function_info(function: &Function) -> ReadFunctionInfo {
    ReadFunctionInfo {
        name: function.name.clone(),
        signature: function.signature(),
        selector: format!("0x{}", hex::encode(function.selector())),
        state_mutability: function.state_mutability.as_json_str().to_string(),
        inputs: function
            .inputs
            .iter()
            .map(|param| ContractFunctionArg {
                name: param.name.clone(),
                kind: param.selector_type().into_owned(),
            })
            .collect(),
        outputs: function
            .outputs
            .iter()
            .map(|param| ContractFunctionArg {
                name: param.name.clone(),
                kind: param.selector_type().into_owned(),
            })
            .collect(),
    }
}

fn find_read_function_by_selector(abi: &JsonAbi, selector: &str) -> Option<Function> {
    let normalized = selector.trim().trim_start_matches("0x").to_lowercase();
    abi.functions
        .values()
        .flat_map(|functions| functions.iter())
        .find(|function| {
            matches!(
                function.state_mutability,
                StateMutability::View | StateMutability::Pure
            ) && hex::encode(function.selector()) == normalized
        })
        .cloned()
}

fn coerce_read_args(function: &Function, args: &[String]) -> Result<Vec<DynSolValue>, String> {
    if function.inputs.len() != args.len() {
        return Err(format!(
            "Expected {} arguments, received {}",
            function.inputs.len(),
            args.len()
        ));
    }

    function
        .inputs
        .iter()
        .zip(args.iter())
        .map(|(param, value)| {
            param.resolve()
                .map_err(|e| e.to_string())?
                .coerce_str(value)
                .map_err(|e| e.to_string())
        })
        .collect()
}

fn dyn_value_to_string(value: &DynSolValue) -> String {
    match value {
        DynSolValue::Bool(inner) => inner.to_string(),
        DynSolValue::Int(inner, _) => inner.to_string(),
        DynSolValue::Uint(inner, _) => inner.to_string(),
        DynSolValue::FixedBytes(inner, _) => format!("0x{}", hex::encode(inner)),
        DynSolValue::Address(inner) => inner.to_string().to_ascii_lowercase(),
        DynSolValue::Function(inner) => format!("0x{}", hex::encode(inner.as_slice())),
        DynSolValue::Bytes(inner) => format!("0x{}", hex::encode(inner)),
        DynSolValue::String(inner) => inner.clone(),
        DynSolValue::Array(inner) | DynSolValue::FixedArray(inner) | DynSolValue::Tuple(inner) => {
            let values: Vec<String> = inner.iter().map(dyn_value_to_string).collect();
            format!("[{}]", values.join(", "))
        }
    }
}

fn normalize_search_input(value: &str) -> String {
    if value.starts_with("0x") {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    }
}

fn direct_search_hit(value: &str) -> Option<SearchHit> {
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        let block_num = i64::from_str(value).ok()?;
        return Some(SearchHit {
            entity_type: "block".to_string(),
            address: None,
            block_num: Some(block_num),
            tx_hash: None,
            title: format!("Block {block_num}"),
            subtitle: None,
            href: format!("/explore/block/{block_num}"),
        });
    }

    let normalized = if value.starts_with("0x") {
        value.to_ascii_lowercase()
    } else {
        format!("0x{}", value.to_ascii_lowercase())
    };

    if normalized.len() == 42 && Address::from_str(&normalized).is_ok() {
        return Some(SearchHit {
            entity_type: "address".to_string(),
            address: Some(normalized.clone()),
            block_num: None,
            tx_hash: None,
            title: normalized.clone(),
            subtitle: Some("Address".to_string()),
            href: format!("/explore/address/{normalized}"),
        });
    }

    if normalized.len() == 66 && normalized[2..].chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Some(SearchHit {
            entity_type: "tx".to_string(),
            address: None,
            block_num: None,
            tx_hash: Some(normalized.clone()),
            title: normalized.clone(),
            subtitle: Some("Transaction".to_string()),
            href: format!("/explore/receipt/{normalized}"),
        });
    }

    None
}

fn optional_non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
