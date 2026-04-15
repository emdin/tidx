use std::time::Duration;

use anyhow::Result;
use kaspa_wrpc_client::{
    KaspaRpcClient, WrpcEncoding,
    client::{ConnectOptions, ConnectStrategy},
    prelude::{NetworkId, NetworkType},
};

pub async fn connect_borsh_wrpc(url: &str) -> Result<KaspaRpcClient> {
    let client = KaspaRpcClient::new(
        WrpcEncoding::Borsh,
        Some(url),
        None,
        Some(NetworkId::new(NetworkType::Mainnet)),
        None,
    )?;

    client
        .connect(Some(ConnectOptions {
            block_async_connect: true,
            connect_timeout: Some(Duration::from_secs(10)),
            strategy: ConnectStrategy::Fallback,
            ..Default::default()
        }))
        .await?;

    Ok(client)
}
