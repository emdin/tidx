//! `tidx watch-tip` — follow each RPC provider's tip and record every reorg it
//! makes, at any depth, into the `tipwatch` schema. See `tidx::tipwatch`.

use anyhow::{Context, Result, anyhow, bail};
use clap::Args as ClapArgs;
use std::collections::HashSet;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinSet;
use tracing::{error, info};

use tidx::db;
use tidx::tipwatch::{TipRpc, WatchOptions, Watcher, store};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Provider to follow, as NAME=URL. Repeat for several; each is followed on
    /// its own, so providers that disagree never look like a reorg.
    #[arg(long = "provider", value_name = "NAME=URL", required = true)]
    pub providers: Vec<String>,

    /// Request budget for one provider, as NAME=RPS. Providers not listed get
    /// --default-rps.
    #[arg(long = "rps", value_name = "NAME=RPS")]
    pub rps: Vec<String>,

    #[arg(long, default_value_t = 2.0)]
    pub default_rps: f64,

    /// Postgres URL without a password, e.g.
    /// postgres://tidx_tipwatch@postgres:5432/tidx_igra
    #[arg(long)]
    pub pg_url: String,

    /// Environment variable that holds the Postgres password. The password
    /// stays off the command line, which every local user can read.
    #[arg(long)]
    pub pg_password_env: Option<String>,

    /// Refuse to start unless every provider reports this chain id.
    #[arg(long)]
    pub chain_id: Option<u64>,

    /// Heights of each provider's chain kept in memory and in tipwatch.seen.
    #[arg(long, default_value_t = WatchOptions::default().memory_blocks)]
    pub memory_blocks: usize,

    /// Most parents fetched by hash in one poll before falling back to a
    /// by-number comparison and recording the skipped heights as a gap.
    #[arg(long, default_value_t = WatchOptions::default().max_walk)]
    pub max_walk: usize,
}

#[derive(Debug, PartialEq)]
struct ProviderPlan {
    name: String,
    url: String,
    rps: f64,
}

fn split_pair<'a>(what: &str, s: &'a str) -> Result<(&'a str, &'a str)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("{what} must be NAME=VALUE, got {s:?}"))?;
    if k.is_empty()
        || !k
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("{what} name must be letters, digits, '-' or '_', got {k:?}");
    }
    Ok((k, v))
}

fn plan_providers(args: &Args) -> Result<Vec<ProviderPlan>> {
    let mut rps = std::collections::HashMap::new();
    for r in &args.rps {
        let (name, v) = split_pair("--rps", r)?;
        let n: f64 = v
            .parse()
            .with_context(|| format!("--rps {name}: not a number: {v:?}"))?;
        if !(n > 0.0 && n.is_finite()) {
            bail!("--rps {name} must be positive, got {v}");
        }
        rps.insert(name.to_string(), n);
    }
    let mut names = HashSet::new();
    let mut plans = Vec::new();
    for p in &args.providers {
        let (name, url) = split_pair("--provider", p)?;
        let parsed = url::Url::parse(url).with_context(|| format!("--provider {name}: bad URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("--provider {name}: only http(s) URLs are supported");
        }
        if !names.insert(name.to_string()) {
            bail!("--provider {name} given twice");
        }
        plans.push(ProviderPlan {
            name: name.to_string(),
            url: url.to_string(),
            rps: rps.remove(name).unwrap_or(args.default_rps),
        });
    }
    if let Some(unknown) = rps.keys().next() {
        bail!("--rps {unknown}: no such --provider");
    }
    Ok(plans)
}

fn resolve_pg_url(
    pg_url: &str,
    password_env: Option<&str>,
    getenv: impl Fn(&str) -> Result<String, std::env::VarError>,
) -> Result<String> {
    let mut url = url::Url::parse(pg_url).context("bad --pg-url")?;
    if url.password().is_some() {
        bail!(
            "--pg-url must not contain a password (command lines are readable by every local user); use --pg-password-env"
        );
    }
    if let Some(var) = password_env {
        let pw = getenv(var).with_context(|| format!("--pg-password-env {var} is not set"))?;
        url.set_password(Some(&pw))
            .map_err(|()| anyhow!("cannot set a password on --pg-url"))?;
    }
    Ok(url.to_string())
}

pub async fn run(args: Args) -> Result<()> {
    let plans = plan_providers(&args)?;
    let opts = WatchOptions {
        memory_blocks: args.memory_blocks,
        max_walk: args.max_walk,
    };
    let pg_url = resolve_pg_url(&args.pg_url, args.pg_password_env.as_deref(), |v| {
        std::env::var(v)
    })?;
    let pool = db::create_pool_with_size(&pg_url, 2 + plans.len()).await?;
    store::ensure_schema(&pool).await?;

    let mut tasks = JoinSet::new();
    for p in plans {
        let rpc = TipRpc::new(&p.url, p.rps)?;
        let id = rpc
            .chain_id()
            .await
            .with_context(|| format!("provider {}: eth_chainId", p.name))?;
        if let Some(want) = args.chain_id
            && id != want
        {
            bail!("provider {} is on chain {id}, expected {want}", p.name);
        }
        let watcher = Watcher::start(&p.name, rpc, pool.clone(), opts).await?;
        info!(provider = %p.name, url = %p.url, rps = p.rps, chain_id = id, "tip watch started");
        tasks.spawn(watcher.run());
    }

    let mut term = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => info!("SIGTERM, stopping"),
        _ = tokio::signal::ctrl_c() => info!("interrupted, stopping"),
        ended = tasks.join_next() => {
            let reason = match ended {
                Some(Ok(Ok(()))) => "returned".to_string(),
                Some(Ok(Err(e))) => format!("{e:#}"),
                Some(Err(e)) => e.to_string(),
                None => "no providers".to_string(),
            };
            error!(%reason, "a tip watcher stopped");
            bail!("tip watcher stopped: {reason}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(providers: &[&str], rps: &[&str]) -> Args {
        Args {
            providers: providers.iter().map(|s| s.to_string()).collect(),
            rps: rps.iter().map(|s| s.to_string()).collect(),
            default_rps: 2.0,
            pg_url: "postgres://tidx_tipwatch@postgres:5432/tidx_igra".into(),
            pg_password_env: None,
            chain_id: None,
            memory_blocks: 100,
            max_walk: 10,
        }
    }

    #[test]
    fn plans_each_provider_with_its_own_rate() {
        let plans = plan_providers(&args(
            &[
                "own=https://igra-mainnet.example",
                "official=https://rpc.example:8545",
            ],
            &["own=4"],
        ))
        .unwrap();
        assert_eq!(
            plans,
            vec![
                ProviderPlan {
                    name: "own".into(),
                    url: "https://igra-mainnet.example".into(),
                    rps: 4.0
                },
                ProviderPlan {
                    name: "official".into(),
                    url: "https://rpc.example:8545".into(),
                    rps: 2.0
                },
            ]
        );
    }

    #[test]
    fn rejects_malformed_providers_and_rates() {
        for (providers, rps, expect) in [
            (vec!["https://no-name.example"], vec![], "NAME=VALUE"),
            (vec!["=https://x.example"], vec![], "name must be"),
            (vec!["a b=https://x.example"], vec![], "name must be"),
            (vec!["own=ws://x.example"], vec![], "http(s)"),
            (vec!["own=not a url"], vec![], "bad URL"),
            (
                vec!["own=https://a.example", "own=https://b.example"],
                vec![],
                "twice",
            ),
            (vec!["own=https://a.example"], vec!["own=0"], "positive"),
            (vec!["own=https://a.example"], vec!["own=-1"], "positive"),
            (vec!["own=https://a.example"], vec!["own=NaN"], "positive"),
            (
                vec!["own=https://a.example"],
                vec!["own=fast"],
                "not a number",
            ),
            (
                vec!["own=https://a.example"],
                vec!["other=2"],
                "no such --provider",
            ),
        ] {
            let err = plan_providers(&args(&providers, &rps)).expect_err(expect);
            assert!(
                format!("{err:#}").contains(expect),
                "{providers:?} {rps:?}: {err:#}"
            );
        }
    }

    #[test]
    fn password_never_comes_from_the_command_line() {
        let env = |v: &str| match v {
            "PW" => Ok("s3cret".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        };
        let err = resolve_pg_url(
            "postgres://tidx_tipwatch:secret@postgres/tidx_igra",
            None,
            env,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not contain a password"));

        let err = resolve_pg_url(
            "postgres://tidx_tipwatch@postgres/tidx_igra",
            Some("UNSET"),
            env,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("is not set"));

        let url = resolve_pg_url(
            "postgres://tidx_tipwatch@postgres:5432/tidx_igra",
            Some("PW"),
            env,
        )
        .unwrap();
        assert_eq!(
            url,
            "postgres://tidx_tipwatch:s3cret@postgres:5432/tidx_igra"
        );
    }
}
