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
    compiler_settings: Option<String>,
    #[serde(rename = "CompilerVersion", default)]
    compiler_version: Option<String>,
    #[serde(rename = "ContractName", default)]
    contract_name: Option<String>,
    #[serde(rename = "EVMVersion", default)]
    evm_version: Option<String>,
    #[serde(rename = "FileName", default)]
    file_name: Option<String>,
    #[serde(rename = "IsProxy", default)]
    is_proxy: Option<String>,
    #[serde(rename = "OptimizationRuns", default)]
    optimization_runs: Option<String>,
    #[serde(rename = "OptimizationUsed", default)]
    optimization_used: Option<String>,
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
    has_source_code: bool,
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

pub async fn run(args: Args) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let mut addresses = Vec::new();
    if let Some(address) = args.address.as_ref() {
        addresses.push(normalize_address(address)?);
    } else {
        addresses = fetch_verified_addresses(&client, &args.source_url, args.page_size, args.max_contracts)
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

        let response: BlockscoutListContractsResponse =
            client.get(url).send().await?.error_for_status()?.json().await?;

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

async fn import_one(
    client: &reqwest::Client,
    args: &Args,
    address: &str,
) -> Result<ImportOutcome> {
    if !args.overwrite && local_has_source_code(client, &args.local_url, args.chain_id, address).await? {
        return Ok(ImportOutcome::Skipped("already imported"));
    }

    let source_entry = fetch_blockscout_source(client, &args.source_url, address).await?;
    let abi: Value = serde_json::from_str(source_entry.abi.trim())
        .with_context(|| format!("Invalid ABI for {address}"))?;
    let contract_name = source_entry
        .contract_name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "ImportedContract".to_string());
    let source_code = compose_source_code(&source_entry);
    let request = VerifyContractRequest {
        contract_name,
        abi,
        runtime_bytecode: None,
        source_code,
        language: Some("Solidity".to_string()),
        compiler_version: source_entry.compiler_version.clone(),
        optimization_enabled: parse_boolish(source_entry.optimization_used.as_deref()),
        optimization_runs: source_entry
            .optimization_runs
            .as_deref()
            .and_then(|value| value.parse::<i32>().ok()),
        license: None,
        constructor_args: None,
        metadata: Some(json!({
            "source": "blockscout",
            "source_explorer": args.source_url.trim_end_matches('/'),
            "imported_address": address,
            "blockscout": source_entry,
            "compiler_settings": source_entry
                .compiler_settings
                .as_deref()
                .and_then(parse_jsonish),
        })),
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

async fn local_has_source_code(
    client: &reqwest::Client,
    local_url: &str,
    chain_id: u64,
    address: &str,
) -> Result<bool> {
    let url = format!(
        "{}/explore/api/contract/{}/verification?chainId={}",
        local_url.trim_end_matches('/'),
        address,
        chain_id
    );
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Ok(false);
    }
    let payload: LocalVerificationResponse = response.json().await?;
    Ok(payload.ok
        && payload
            .verification
            .as_ref()
            .map(|detail| detail.summary.has_source_code)
            .unwrap_or(false))
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

    let response: BlockscoutSourceResponse =
        client.get(url).send().await?.error_for_status()?.json().await?;
    if response.status != "1" {
        anyhow::bail!("Blockscout getsourcecode failed for {}: {}", address, response.message);
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

fn parse_boolish(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn parse_jsonish(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
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
