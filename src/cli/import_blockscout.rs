use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

#[derive(ClapArgs)]
pub struct Args {
    /// Local explorer API base URL. Use localhost inside the running tidx container.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub local_url: String,

    /// Source Blockscout explorer base URL.
    #[arg(long, default_value = "https://explorer.igralabs.com")]
    pub source_url: String,

    /// Chain ID to import into.
    #[arg(long, default_value_t = 38833)]
    pub chain_id: u64,

    /// Verified contracts page size for Blockscout pagination.
    #[arg(long, default_value_t = 50)]
    pub page_size: usize,

    /// Stop after importing this many contracts.
    #[arg(long)]
    pub max_contracts: Option<usize>,

    /// Import a single contract address instead of paging all verified contracts.
    #[arg(long)]
    pub address: Option<String>,

    /// Re-import contracts even if local source code already exists.
    #[arg(long)]
    pub overwrite: bool,
}

#[derive(Debug, Deserialize)]
struct BlockscoutListContractsResponse {
    status: String,
    message: String,
    #[serde(default)]
    result: Vec<BlockscoutListContractItem>,
}

#[derive(Debug, Deserialize)]
struct BlockscoutListContractItem {
    #[serde(rename = "Address")]
    address: String,
}

#[derive(Debug, Deserialize)]
struct BlockscoutSourceResponse {
    status: String,
    message: String,
    #[serde(default)]
    result: Vec<BlockscoutSourceEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BlockscoutSourceEntry {
    #[serde(rename = "ABI")]
    abi: String,
    #[serde(rename = "AdditionalSources", default)]
    additional_sources: Vec<BlockscoutAdditionalSource>,
    #[serde(rename = "Address")]
    address: String,
    #[serde(rename = "CompilerSettings", default)]
    compiler_settings: Option<Value>,
    #[serde(rename = "CompilerVersion", default)]
    compiler_version: Option<String>,
    #[serde(rename = "ContractName", default)]
    contract_name: Option<String>,
    #[serde(rename = "EVMVersion", default)]
    evm_version: Option<String>,
    #[serde(rename = "FileName", default)]
    file_name: Option<String>,
    #[serde(rename = "IsProxy", default)]
    is_proxy: Option<Value>,
    #[serde(rename = "OptimizationRuns", default)]
    optimization_runs: Option<Value>,
    #[serde(rename = "OptimizationUsed", default)]
    optimization_used: Option<Value>,
    #[serde(rename = "SourceCode", default)]
    source_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BlockscoutAdditionalSource {
    #[serde(rename = "Filename")]
    filename: String,
    #[serde(rename = "SourceCode")]
    source_code: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct BlockscoutV2Contract {
    #[serde(default)]
    abi: Option<Value>,
    #[serde(default)]
    additional_sources: Vec<BlockscoutV2AdditionalSource>,
    #[serde(default)]
    compiler_settings: Option<Value>,
    #[serde(default)]
    compiler_version: Option<String>,
    #[serde(default)]
    constructor_args: Option<String>,
    #[serde(default)]
    creation_bytecode: Option<String>,
    #[serde(default)]
    deployed_bytecode: Option<String>,
    #[serde(default)]
    evm_version: Option<String>,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    is_changed_bytecode: Option<bool>,
    #[serde(default)]
    is_fully_verified: Option<bool>,
    #[serde(default)]
    is_partially_verified: Option<bool>,
    #[serde(default)]
    is_verified: Option<bool>,
    #[serde(default)]
    is_verified_via_eth_bytecode_db: Option<bool>,
    #[serde(default)]
    is_verified_via_sourcify: Option<bool>,
    #[serde(default)]
    is_verified_via_verifier_alliance: Option<bool>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    license_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    optimization_enabled: Option<Value>,
    #[serde(default)]
    optimization_runs: Option<Value>,
    #[serde(default)]
    source_code: Option<String>,
    #[serde(default)]
    verified_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BlockscoutV2AdditionalSource {
    file_path: String,
    source_code: String,
}

#[derive(Debug, Deserialize)]
struct LocalVerificationResponse {
    ok: bool,
    #[serde(default)]
    verification: Option<LocalVerificationDetail>,
}

#[derive(Debug, Deserialize)]
struct LocalVerificationDetail {
    summary: LocalVerificationSummary,
}

#[derive(Debug, Deserialize)]
struct LocalVerificationSummary {
    #[serde(default)]
    has_source_code: bool,
    #[serde(default)]
    has_runtime_bytecode: bool,
}

#[derive(Debug)]
struct LocalImportState {
    has_source_code: bool,
    has_runtime_bytecode: bool,
}

#[derive(Debug, Serialize)]
struct VerifyContractRequest {
    contract_name: String,
    abi: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_bytecode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compiler_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optimization_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optimization_runs: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    constructor_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Value>,
}

struct ImportedContractData {
    contract_name: String,
    abi: Value,
    runtime_bytecode: Option<String>,
    source_code: Option<String>,
    language: Option<String>,
    compiler_version: Option<String>,
    optimization_enabled: Option<bool>,
    optimization_runs: Option<i32>,
    license: Option<String>,
    constructor_args: Option<String>,
    metadata: Option<Value>,
}

pub async fn run(args: Args) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let mut addresses = Vec::new();
    if let Some(address) = args.address.as_ref() {
        addresses.push(normalize_address(address)?);
    } else {
        addresses = fetch_verified_addresses(
            &client,
            &args.source_url,
            args.page_size,
            args.max_contracts,
        )
        .await?;
    }

    if addresses.is_empty() {
        println!("No verified contracts found to import.");
        return Ok(());
    }

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for address in addresses {
        match import_one(&client, &args, &address).await {
            Ok(ImportOutcome::Imported) => {
                imported += 1;
                println!("imported {}", address);
            }
            Ok(ImportOutcome::Skipped(reason)) => {
                skipped += 1;
                println!("skipped {} ({})", address, reason);
            }
            Err(error) => {
                failed += 1;
                warn!(address = %address, error = %error, "Blockscout import failed");
                println!("failed {} ({})", address, error);
            }
        }
    }

    println!(
        "Blockscout import complete: imported={}, skipped={}, failed={}",
        imported, skipped, failed
    );

    Ok(())
}

#[derive(Debug)]
enum ImportOutcome {
    Imported,
    Skipped(&'static str),
}

async fn fetch_verified_addresses(
    client: &reqwest::Client,
    source_url: &str,
    page_size: usize,
    max_contracts: Option<usize>,
) -> Result<Vec<String>> {
    let mut addresses = Vec::new();
    let mut page = 1usize;

    loop {
        let mut url = reqwest::Url::parse(&format!("{}/api", source_url.trim_end_matches('/')))?;
        url.query_pairs_mut()
            .append_pair("module", "contract")
            .append_pair("action", "listcontracts")
            .append_pair("filter", "verified")
            .append_pair("page", &page.to_string())
            .append_pair("offset", &page_size.to_string());

        let response: BlockscoutListContractsResponse = client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if response.status != "1" {
            anyhow::bail!("Blockscout listcontracts failed: {}", response.message);
        }

        if response.result.is_empty() {
            break;
        }

        for item in response.result {
            addresses.push(normalize_address(&item.address)?);
            if let Some(limit) = max_contracts
                && addresses.len() >= limit
            {
                return Ok(addresses);
            }
        }

        page += 1;
    }

    Ok(addresses)
}

async fn import_one(client: &reqwest::Client, args: &Args, address: &str) -> Result<ImportOutcome> {
    let existing = local_import_state(client, &args.local_url, args.chain_id, address).await?;
    let imported = fetch_import_data(client, args, address).await?;

    if !args.overwrite
        && existing.has_source_code
        && (existing.has_runtime_bytecode || imported.runtime_bytecode.is_none())
    {
        return Ok(ImportOutcome::Skipped("already imported"));
    }

    let request = VerifyContractRequest {
        contract_name: imported.contract_name,
        abi: imported.abi,
        runtime_bytecode: imported.runtime_bytecode,
        source_code: imported.source_code,
        language: imported.language,
        compiler_version: imported.compiler_version,
        optimization_enabled: imported.optimization_enabled,
        optimization_runs: imported.optimization_runs,
        license: imported.license,
        constructor_args: imported.constructor_args,
        metadata: imported.metadata,
    };

    let url = format!(
        "{}/explore/api/contract/{}/verify?chainId={}",
        args.local_url.trim_end_matches('/'),
        address,
        args.chain_id
    );
    let response = client.post(url).json(&request).send().await?;
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Local verify endpoint rejected import: {}", body);
    }

    info!(address = %address, "Imported verified contract from Blockscout");
    Ok(ImportOutcome::Imported)
}

async fn local_import_state(
    client: &reqwest::Client,
    local_url: &str,
    chain_id: u64,
    address: &str,
) -> Result<LocalImportState> {
    let url = format!(
        "{}/explore/api/contract/{}/verification?chainId={}",
        local_url.trim_end_matches('/'),
        address,
        chain_id
    );
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Ok(LocalImportState {
            has_source_code: false,
            has_runtime_bytecode: false,
        });
    }
    let payload: LocalVerificationResponse = response.json().await?;
    let summary = payload.verification.as_ref().map(|detail| &detail.summary);
    Ok(LocalImportState {
        has_source_code: payload.ok
            && summary
                .map(|summary| summary.has_source_code)
                .unwrap_or(false),
        has_runtime_bytecode: payload.ok
            && summary
                .map(|summary| summary.has_runtime_bytecode)
                .unwrap_or(false),
    })
}

async fn fetch_import_data(
    client: &reqwest::Client,
    args: &Args,
    address: &str,
) -> Result<ImportedContractData> {
    match fetch_blockscout_v2_contract(client, &args.source_url, address).await {
        Ok(detail) => match build_import_data_from_v2(args, address, &detail) {
            Ok(imported) => Ok(imported),
            Err(error) => {
                warn!(
                    address = %address,
                    error = %error,
                    "Blockscout v2 payload was incomplete, falling back to legacy getsourcecode"
                );
                fetch_import_data_from_v1(client, args, address).await
            }
        },
        Err(error) => {
            warn!(
                address = %address,
                error = %error,
                "Blockscout v2 fetch failed, falling back to legacy getsourcecode"
            );
            fetch_import_data_from_v1(client, args, address).await
        }
    }
}

async fn fetch_import_data_from_v1(
    client: &reqwest::Client,
    args: &Args,
    address: &str,
) -> Result<ImportedContractData> {
    let source_entry = fetch_blockscout_source(client, &args.source_url, address).await?;
    build_import_data_from_v1(args, address, source_entry)
}

async fn fetch_blockscout_v2_contract(
    client: &reqwest::Client,
    source_url: &str,
    address: &str,
) -> Result<BlockscoutV2Contract> {
    let url = format!(
        "{}/api/v2/smart-contracts/{}",
        source_url.trim_end_matches('/'),
        address
    );
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn build_import_data_from_v2(
    args: &Args,
    address: &str,
    detail: &BlockscoutV2Contract,
) -> Result<ImportedContractData> {
    let abi = detail
        .abi
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Blockscout v2 response did not include ABI"))?;
    let contract_name = detail
        .name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "ImportedContract".to_string());
    let source_code = compose_v2_source_code(detail);
    let runtime_bytecode = non_empty_hex_blob(detail.deployed_bytecode.as_deref());
    let runtime_bytecode_available = runtime_bytecode.is_some();
    let creation_bytecode_available =
        non_empty_hex_blob(detail.creation_bytecode.as_deref()).is_some();

    Ok(ImportedContractData {
        contract_name,
        abi,
        runtime_bytecode,
        source_code,
        language: detail
            .language
            .clone()
            .or_else(|| Some("Solidity".to_string())),
        compiler_version: detail.compiler_version.clone(),
        optimization_enabled: detail
            .optimization_enabled
            .as_ref()
            .and_then(parse_boolish_value),
        optimization_runs: detail.optimization_runs.as_ref().and_then(parse_i32_value),
        license: detail.license_type.clone(),
        constructor_args: non_empty_hex_blob(detail.constructor_args.as_deref()),
        metadata: Some(json!({
            "source": "blockscout_v2",
            "source_explorer": args.source_url.trim_end_matches('/'),
            "imported_address": address,
            "api_path": format!("/api/v2/smart-contracts/{address}"),
            "file_path": detail.file_path,
            "evm_version": detail.evm_version,
            "verified_at": detail.verified_at,
            "is_verified": detail.is_verified,
            "is_fully_verified": detail.is_fully_verified,
            "is_partially_verified": detail.is_partially_verified,
            "is_changed_bytecode": detail.is_changed_bytecode,
            "is_verified_via_eth_bytecode_db": detail.is_verified_via_eth_bytecode_db,
            "is_verified_via_sourcify": detail.is_verified_via_sourcify,
            "is_verified_via_verifier_alliance": detail.is_verified_via_verifier_alliance,
            "compiler_settings": detail.compiler_settings,
            "creation_bytecode_available": creation_bytecode_available,
            "deployed_bytecode_available": runtime_bytecode_available,
        })),
    })
}

fn build_import_data_from_v1(
    args: &Args,
    address: &str,
    source_entry: BlockscoutSourceEntry,
) -> Result<ImportedContractData> {
    let abi: Value = serde_json::from_str(source_entry.abi.trim())
        .with_context(|| format!("Invalid ABI for {address}"))?;
    let contract_name = source_entry
        .contract_name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "ImportedContract".to_string());
    let source_code = compose_source_code(&source_entry);
    let compiler_settings = source_entry.compiler_settings.clone();

    Ok(ImportedContractData {
        contract_name,
        abi,
        runtime_bytecode: None,
        source_code,
        language: Some("Solidity".to_string()),
        compiler_version: source_entry.compiler_version.clone(),
        optimization_enabled: source_entry
            .optimization_used
            .as_ref()
            .and_then(parse_boolish_value),
        optimization_runs: source_entry
            .optimization_runs
            .as_ref()
            .and_then(parse_i32_value),
        license: None,
        constructor_args: None,
        metadata: Some(json!({
            "source": "blockscout",
            "source_explorer": args.source_url.trim_end_matches('/'),
            "imported_address": address,
            "blockscout": source_entry,
            "compiler_settings": compiler_settings,
        })),
    })
}

async fn fetch_blockscout_source(
    client: &reqwest::Client,
    source_url: &str,
    address: &str,
) -> Result<BlockscoutSourceEntry> {
    let mut url = reqwest::Url::parse(&format!("{}/api", source_url.trim_end_matches('/')))?;
    url.query_pairs_mut()
        .append_pair("module", "contract")
        .append_pair("action", "getsourcecode")
        .append_pair("address", address);

    let response: BlockscoutSourceResponse = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if response.status != "1" {
        anyhow::bail!(
            "Blockscout getsourcecode failed for {}: {}",
            address,
            response.message
        );
    }
    response
        .result
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No Blockscout source payload returned for {}", address))
}

fn compose_source_code(entry: &BlockscoutSourceEntry) -> Option<String> {
    let main = entry
        .source_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let additional = entry
        .additional_sources
        .iter()
        .filter(|item| !item.source_code.trim().is_empty())
        .map(|item| format!("// File: {}\n{}", item.filename, item.source_code.trim()))
        .collect::<Vec<_>>();

    match (main, additional.is_empty()) {
        (None, true) => None,
        (Some(main), true) => Some(main),
        (main, false) => {
            let mut sections = Vec::new();
            if let Some(main) = main {
                let file_name = entry.file_name.as_deref().unwrap_or("main.sol");
                sections.push(format!("// File: {}\n{}", file_name, main.trim()));
            }
            sections.extend(additional);
            Some(sections.join("\n\n"))
        }
    }
}

fn compose_v2_source_code(entry: &BlockscoutV2Contract) -> Option<String> {
    let main = entry
        .source_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let additional = entry
        .additional_sources
        .iter()
        .filter(|item| !item.source_code.trim().is_empty())
        .map(|item| format!("// File: {}\n{}", item.file_path, item.source_code.trim()))
        .collect::<Vec<_>>();

    match (main, additional.is_empty()) {
        (None, true) => None,
        (Some(main), true) => Some(main),
        (main, false) => {
            let mut sections = Vec::new();
            if let Some(main) = main {
                let file_name = entry.file_path.as_deref().unwrap_or("main.sol");
                sections.push(format!("// File: {}\n{}", file_name, main.trim()));
            }
            sections.extend(additional);
            Some(sections.join("\n\n"))
        }
    }
}

fn non_empty_hex_blob(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    let body = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if body.is_empty() || !body.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", body.to_ascii_lowercase()))
}

fn parse_boolish_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(inner) => Some(*inner),
        Value::Number(inner) => inner.as_i64().map(|number| number != 0),
        Value::String(inner) => match inner.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn parse_i32_value(value: &Value) -> Option<i32> {
    match value {
        Value::Number(inner) => inner.as_i64().and_then(|number| i32::try_from(number).ok()),
        Value::String(inner) => inner.trim().parse::<i32>().ok(),
        _ => None,
    }
}

fn normalize_address(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let with_prefix = if trimmed.starts_with("0x") {
        trimmed.to_ascii_lowercase()
    } else {
        format!("0x{}", trimmed.to_ascii_lowercase())
    };
    if with_prefix.len() == 42 && with_prefix[2..].chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(with_prefix)
    } else {
        anyhow::bail!("Invalid address: {}", value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_args() -> Args {
        Args {
            local_url: "http://127.0.0.1:8080".to_string(),
            source_url: "https://explorer.igralabs.com".to_string(),
            chain_id: 38833,
            page_size: 50,
            max_contracts: None,
            address: None,
            overwrite: false,
        }
    }

    #[test]
    fn v2_import_maps_deployed_bytecode_to_runtime_bytecode() {
        let detail = BlockscoutV2Contract {
            abi: Some(json!([{"type": "function", "name": "ping"}])),
            compiler_version: Some("v0.8.30+commit.73712a01".to_string()),
            deployed_bytecode: Some("0xAABBcc".to_string()),
            file_path: Some("src/Ping.sol".to_string()),
            name: Some("Ping".to_string()),
            optimization_enabled: Some(json!(true)),
            optimization_runs: Some(json!(200)),
            source_code: Some("contract Ping {}".to_string()),
            ..Default::default()
        };

        let imported = build_import_data_from_v2(
            &test_args(),
            "0x0000000000000000000000000000000000000001",
            &detail,
        )
        .unwrap();

        assert_eq!(imported.contract_name, "Ping");
        assert_eq!(imported.runtime_bytecode.as_deref(), Some("0xaabbcc"));
        assert_eq!(imported.optimization_enabled, Some(true));
        assert_eq!(imported.optimization_runs, Some(200));
    }

    #[test]
    fn empty_or_invalid_v2_bytecode_is_not_submitted() {
        assert_eq!(non_empty_hex_blob(Some("0x")), None);
        assert_eq!(non_empty_hex_blob(Some("not-hex")), None);
        assert_eq!(non_empty_hex_blob(Some("aabb")), Some("0xaabb".to_string()));
    }
}
