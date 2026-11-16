use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use super::rpc::{Header, TipRpc};
use super::store::{self, NewReorg, Seen};
use crate::db::Pool;

#[derive(Clone, Copy, Debug)]
pub struct WatchOptions {
    /// Heights of each provider's chain kept in memory and in `tipwatch.seen`.
    pub memory_blocks: usize,
    /// Most parents fetched by hash in one step. A tip further ahead than this
    /// is checked by block number instead and the skipped heights become a gap.
    pub max_walk: usize,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            memory_blocks: 20_000,
            max_walk: 256,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// Same tip as before, or the provider answered with an older block it
    /// already reported (a lagging backend node).
    Unchanged,
    Extended {
        blocks: usize,
        tip: u64,
    },
    Reorg {
        id: i64,
        depth: u32,
        fork_point: u64,
    },
    /// The tip moved further than `max_walk`. The old tip was compared with the
    /// provider's block at the same height (`reorg` if it changed) and the
    /// heights in between were recorded as a gap.
    Jump {
        reorg: Option<(i64, u32)>,
        gap_from: u64,
        gap_to: u64,
    },
    /// No common block within memory: recorded as a gap, chain restarted.
    Diverged {
        gap_from: u64,
        gap_to: u64,
    },
}

/// The provider's chain as last seen: contiguous and hash-linked.
struct Chain {
    blocks: BTreeMap<u64, Seen>,
    memory: usize,
}

impl Chain {
    /// Keeps the hash-linked run that ends at the highest loaded height; rows
    /// below a gap belong to an earlier stretch the watcher can't vouch for.
    fn from_loaded(rows: Vec<Seen>, memory: usize) -> Self {
        let mut run: Vec<Seen> = Vec::new();
        for s in rows.into_iter().rev() {
            match run.last() {
                Some(above)
                    if above.header.number != s.header.number + 1
                        || above.header.parent_hash != s.header.hash =>
                {
                    break;
                }
                _ => run.push(s),
            }
        }
        let mut chain = Self {
            blocks: run.into_iter().map(|s| (s.header.number, s)).collect(),
            memory,
        };
        chain.trim();
        chain
    }

    fn tip(&self) -> Option<Seen> {
        self.blocks.values().next_back().copied()
    }

    fn min(&self) -> Option<u64> {
        self.blocks.keys().next().copied()
    }

    fn get(&self, n: u64) -> Option<&Seen> {
        self.blocks.get(&n)
    }

    fn above(&self, n: u64) -> Vec<Seen> {
        self.blocks.range(n + 1..).map(|(_, s)| *s).collect()
    }

    fn replace_above(&mut self, fork: u64, added: &[Seen]) {
        self.blocks.split_off(&(fork + 1));
        self.blocks
            .extend(added.iter().map(|s| (s.header.number, *s)));
        self.trim();
    }

    fn reset(&mut self, head: Seen) {
        self.blocks.clear();
        self.blocks.insert(head.header.number, head);
    }

    fn trim(&mut self) {
        while self.blocks.len() > self.memory {
            self.blocks.pop_first();
        }
    }
}

pub struct Watcher {
    provider: String,
    rpc: TipRpc,
    pool: Pool,
    opts: WatchOptions,
    chain: Chain,
}

impl Watcher {
    /// Loads the provider's last seen chain from `tipwatch.seen`.
    pub async fn start(
        provider: &str,
        rpc: TipRpc,
        pool: Pool,
        opts: WatchOptions,
    ) -> Result<Self> {
        if opts.memory_blocks < 2 || opts.max_walk == 0 {
            bail!("memory_blocks must be at least 2 and max_walk at least 1");
        }
        let rows = store::load_recent(&pool, provider, opts.memory_blocks).await?;
        let chain = Chain::from_loaded(rows, opts.memory_blocks);
        if let Some(tip) = chain.tip() {
            info!(
                provider,
                tip = tip.header.number,
                remembered = chain.blocks.len(),
                "tip watch resuming"
            );
        }
        Ok(Self {
            provider: provider.to_string(),
            rpc,
            pool,
            opts,
            chain,
        })
    }

    pub fn tip(&self) -> Option<Header> {
        self.chain.tip().map(|s| s.header)
    }

    /// One poll: fetch the provider's head, reconcile it with what was seen,
    /// persist the result. Nothing is written or changed when it fails.
    pub async fn step(&mut self) -> Result<StepOutcome> {
        let head = self.rpc.head().await?;
        let now = Utc::now();
        let Some(tip) = self.chain.tip() else {
            let first = Seen {
                header: head,
                seen_at: now,
            };
            store::save_extension(&self.pool, &self.provider, &[first]).await?;
            self.chain.reset(first);
            return Ok(StepOutcome::Extended {
                blocks: 1,
                tip: head.number,
            });
        };
        if self
            .chain
            .get(head.number)
            .is_some_and(|s| s.header.hash == head.hash)
        {
            return Ok(StepOutcome::Unchanged);
        }
        if head.number < self.chain.min().unwrap_or(0) {
            warn!(provider = %self.provider, head = head.number, tip = tip.header.number, "provider head is far behind; ignoring it");
            return Ok(StepOutcome::Unchanged);
        }

        // Walk back by parent hash to a block this provider reported before.
        let mut branch = vec![head];
        let mut cur = head;
        let fork = loop {
            let n = cur
                .number
                .checked_sub(1)
                .ok_or_else(|| anyhow!("walked to genesis without a common block"))?;
            match self.chain.get(n) {
                Some(known) if known.header.hash == cur.parent_hash => break n,
                None if n < self.chain.min().unwrap_or(0) => {
                    return self.compare_by_number(head, tip, now).await;
                }
                _ => {}
            }
            if branch.len() >= self.opts.max_walk {
                return self.compare_by_number(head, tip, now).await;
            }
            let parent = self.rpc.by_hash(cur.parent_hash).await?.ok_or_else(|| {
                anyhow!(
                    "parent {} of block {} not found",
                    cur.parent_hash,
                    cur.number
                )
            })?;
            if parent.number != n {
                bail!(
                    "asked for the parent of block {} at height {n}, got block {}",
                    cur.number,
                    parent.number
                );
            }
            branch.push(parent);
            cur = parent;
        };

        let added: Vec<Seen> = branch
            .into_iter()
            .rev()
            .map(|header| Seen {
                header,
                seen_at: now,
            })
            .collect();
        let orphaned = self.chain.above(fork);
        if orphaned.is_empty() {
            store::save_extension(&self.pool, &self.provider, &added).await?;
            self.chain.replace_above(fork, &added);
            return Ok(StepOutcome::Extended {
                blocks: added.len(),
                tip: head.number,
            });
        }
        let r = NewReorg {
            fork_point: fork,
            old_tip: tip.header.number,
            new_tip: head.number,
            orphaned: &orphaned,
            added: &added,
            after_gap: false,
        };
        let id = store::save_reorg(&self.pool, &self.provider, &r).await?;
        info!(
            provider = %self.provider, id, depth = orphaned.len(), fork_point = fork,
            old_tip = tip.header.number, new_tip = head.number, "tip reorg"
        );
        self.chain.replace_above(fork, &added);
        Ok(StepOutcome::Reorg {
            id,
            depth: orphaned.len() as u32,
            fork_point: fork,
        })
    }

    /// The head is too far from anything remembered to walk to by hash: compare
    /// the remembered chain with the provider's blocks at the same heights,
    /// from the top down, then continue from the head.
    async fn compare_by_number(
        &mut self,
        head: Header,
        tip: Seen,
        now: DateTime<Utc>,
    ) -> Result<StepOutcome> {
        let top = tip.header.number.min(head.number);
        let mut canon: Vec<Header> = Vec::new();
        let mut n = top;
        let fork = loop {
            let Some(known) = self.chain.get(n).copied() else {
                break None;
            };
            let b = self.rpc.by_number(n).await?.ok_or_else(|| {
                anyhow!(
                    "provider has no block {n} although its head is {}",
                    head.number
                )
            })?;
            if b.hash == known.header.hash {
                break Some(n);
            }
            canon.push(b);
            match n.checked_sub(1) {
                Some(below) => n = below,
                None => break None,
            }
        };
        let head_seen = Seen {
            header: head,
            seen_at: now,
        };

        let Some(fork) = fork else {
            let (from, to) = (self.chain.min().unwrap_or(0), head.number.saturating_sub(1));
            store::save_gap(
                &self.pool,
                &self.provider,
                from,
                to,
                "diverged beyond memory: no common block with the provider",
                &head_seen,
            )
            .await?;
            error!(provider = %self.provider, from, to, head = head.number, "tip watch diverged beyond memory");
            self.chain.reset(head_seen);
            return Ok(StepOutcome::Diverged {
                gap_from: from,
                gap_to: to,
            });
        };

        canon.reverse();
        let added: Vec<Seen> = canon
            .into_iter()
            .map(|header| Seen {
                header,
                seen_at: now,
            })
            .collect();
        let top_hash = added
            .last()
            .or_else(|| self.chain.get(top))
            .map(|s| s.header.hash);
        let connects =
            head.number == top || (head.number == top + 1 && Some(head.parent_hash) == top_hash);
        let mut reorg = None;
        if !added.is_empty() {
            let orphaned = self.chain.above(fork);
            let r = NewReorg {
                fork_point: fork,
                old_tip: tip.header.number,
                new_tip: top,
                orphaned: &orphaned,
                added: &added,
                after_gap: !connects,
            };
            let id = store::save_reorg(&self.pool, &self.provider, &r).await?;
            info!(
                provider = %self.provider, id, depth = orphaned.len(), fork_point = fork,
                old_tip = tip.header.number, new_tip = top, "tip reorg (found by block number)"
            );
            self.chain.replace_above(fork, &added);
            reorg = Some((id, orphaned.len() as u32));
        }

        if connects {
            if head.number > top {
                store::save_extension(&self.pool, &self.provider, &[head_seen]).await?;
                self.chain.replace_above(top, &[head_seen]);
            }
            return Ok(match reorg {
                Some((id, depth)) => StepOutcome::Reorg {
                    id,
                    depth,
                    fork_point: fork,
                },
                None => StepOutcome::Extended {
                    blocks: usize::from(head.number > top),
                    tip: head.number,
                },
            });
        }

        let (from, to) = (top + 1, head.number - 1);
        store::save_gap(
            &self.pool,
            &self.provider,
            from,
            to,
            "tip jumped past --max-walk",
            &head_seen,
        )
        .await?;
        warn!(provider = %self.provider, from, to, "tip watch skipped heights");
        self.chain.reset(head_seen);
        Ok(StepOutcome::Jump {
            reorg,
            gap_from: from,
            gap_to: to,
        })
    }

    /// Drops `tipwatch.seen` rows older than the memory window.
    pub async fn prune(&self) -> Result<u64> {
        let Some(tip) = self.chain.tip() else {
            return Ok(0);
        };
        let Some(upto) = tip
            .header
            .number
            .checked_sub(self.opts.memory_blocks as u64)
        else {
            return Ok(0);
        };
        store::prune_seen(&self.pool, &self.provider, upto).await
    }

    /// Steps forever, backing off on errors. RPC and database failures are
    /// logged and retried; they never end the loop.
    pub async fn run(mut self) -> Result<()> {
        const MAX_BACKOFF: Duration = Duration::from_secs(30);
        const EVERY: Duration = Duration::from_secs(600);
        let mut backoff = Duration::from_secs(1);
        let mut last_housekeeping = Instant::now();
        let (mut steps, mut reorgs, mut max_depth, mut errors) = (0u64, 0u64, 0u32, 0u64);

        loop {
            match self.step().await {
                Ok(out) => {
                    steps += 1;
                    backoff = Duration::from_secs(1);
                    match out {
                        StepOutcome::Reorg { depth, .. }
                        | StepOutcome::Jump {
                            reorg: Some((_, depth)),
                            ..
                        } => {
                            reorgs += 1;
                            max_depth = max_depth.max(depth);
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    errors += 1;
                    warn!(provider = %self.provider, error = %format!("{e:#}"), retry_in = ?backoff, "tip watch step failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
            if last_housekeeping.elapsed() >= EVERY {
                match self.prune().await {
                    Ok(n) => debug!(provider = %self.provider, pruned = n, "tip watch pruned seen"),
                    Err(e) => {
                        warn!(provider = %self.provider, error = %format!("{e:#}"), "tip watch prune failed")
                    }
                }
                info!(
                    provider = %self.provider, tip = self.tip().map(|h| h.number), steps, reorgs, max_depth, errors,
                    "tip watch summary (last 10 min)"
                );
                (steps, reorgs, max_depth, errors) = (0, 0, 0, 0);
                last_housekeeping = Instant::now();
            }
        }
    }
}
