use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use super::rpc::{Header, TipRpc};
use super::store::{self, NewReorg, Op, Seen};
use crate::db::Pool;

#[derive(Clone, Copy, Debug)]
pub struct WatchOptions {
    /// Heights of each provider's chain kept in memory and in `tipwatch.seen`.
    pub memory_blocks: usize,
    /// Most blocks fetched one by one in a step (parents by hash, or a new
    /// branch by number). A head further ahead, or a deeper new branch, is
    /// handled by a by-number binary search and the unfetched heights become a
    /// gap.
    pub max_walk: usize,
    /// Days to keep the copies of replaced blocks (`tipwatch.orphans`); the
    /// reorg events themselves are kept forever. `None` keeps everything.
    pub orphan_retention_days: Option<u32>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            memory_blocks: 20_000,
            max_walk: 256,
            orphan_retention_days: Some(90),
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
    /// Some heights were not fetched one by one (the head was more than
    /// `max_walk` ahead, or the new branch was longer than that). The remembered
    /// chain was compared with the provider's blocks by number (`reorg` if part
    /// of it was replaced) and the unfetched heights were recorded as a gap.
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

/// Igra's tip switches between candidate blocks every few seconds (1–2 block
/// reorgs, often flipping back), so only deeper ones are worth a log line by
/// default; all of them are in tipwatch.reorgs. 20 is close to the engine's
/// 30-block head delay.
fn log_reorg(
    provider: &str,
    id: i64,
    depth: usize,
    fork_point: u64,
    old_tip: u64,
    new_tip: u64,
    how: &str,
) {
    match depth {
        20.. => warn!(
            provider,
            id, depth, fork_point, old_tip, new_tip, how, "tip reorg"
        ),
        3.. => info!(
            provider,
            id, depth, fork_point, old_tip, new_tip, how, "tip reorg"
        ),
        _ => debug!(
            provider,
            id, depth, fork_point, old_tip, new_tip, how, "tip reorg"
        ),
    }
}

/// The provider's chain as last seen: contiguous and hash-linked.
struct Chain {
    blocks: BTreeMap<u64, Seen>,
    memory: usize,
}

impl Chain {
    /// Keeps the hash-linked run that ends at the highest loaded height; rows
    /// below a break belong to an earlier stretch the watcher can't vouch for.
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

    /// The remembered header at `n`; callers only ask for heights in memory.
    fn known(&self, n: u64) -> Header {
        self.blocks[&n].header
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
    /// persist the result. Each step commits in one transaction and memory only
    /// changes after it does, so a failed step is simply retried.
    pub async fn step(&mut self) -> Result<StepOutcome> {
        let head = self.rpc.head().await?;
        let now = Utc::now();
        let seen = |header: Header| Seen {
            header,
            seen_at: now,
        };
        let Some(tip) = self.chain.tip() else {
            store::commit(&self.pool, &self.provider, &[Op::Extend(&[seen(head)])]).await?;
            self.chain.reset(seen(head));
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
        let min = self.chain.min().unwrap_or(tip.header.number);
        if head.number < min {
            debug!(provider = %self.provider, head = head.number, min, "provider answered below memory; ignoring it");
            return Ok(StepOutcome::Unchanged);
        }
        if head.number > tip.header.number + self.opts.max_walk as u64 {
            return self.compare_by_number(head, tip, now).await;
        }

        // Walk back by parent hash to a block this provider reported before.
        let mut branch = vec![head];
        let mut cur = head;
        let fork = loop {
            if cur.number == min {
                // `cur` replaces the lowest remembered block. If they share a
                // parent the fork is just below memory (with a one-block memory,
                // right after start or a gap, every flip lands here).
                if min > 0 && self.chain.known(min).parent_hash == cur.parent_hash {
                    break min - 1;
                }
                return self.compare_by_number(head, tip, now).await;
            }
            let n = cur.number - 1;
            if self
                .chain
                .get(n)
                .is_some_and(|k| k.header.hash == cur.parent_hash)
            {
                break n;
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

        let added: Vec<Seen> = branch.into_iter().rev().map(seen).collect();
        let orphaned = self.chain.above(fork);
        if orphaned.is_empty() {
            store::commit(&self.pool, &self.provider, &[Op::Extend(&added)]).await?;
            self.chain.replace_above(fork, &added);
            return Ok(StepOutcome::Extended {
                blocks: added.len(),
                tip: head.number,
            });
        }
        let depth = orphaned.len();
        let ids = store::commit(
            &self.pool,
            &self.provider,
            &[Op::Reorg(NewReorg {
                fork_point: fork,
                old_tip: tip.header.number,
                new_tip: head.number,
                orphaned: &orphaned,
                added: &added,
                after_gap: false,
            })],
        )
        .await?;
        log_reorg(
            &self.provider,
            ids[0],
            depth,
            fork,
            tip.header.number,
            head.number,
            "parent hashes",
        );
        self.chain.replace_above(fork, &added);
        Ok(StepOutcome::Reorg {
            id: ids[0],
            depth: depth as u32,
            fork_point: fork,
        })
    }

    async fn canonical(&self, n: u64, head: u64) -> Result<Header> {
        self.rpc
            .by_number(n)
            .await?
            .ok_or_else(|| anyhow!("provider has no block {n} although its head is {head}"))
    }

    /// The head can't be reached by parent hashes within `max_walk`. Find the
    /// highest remembered height the provider still has (binary search: about
    /// log2(memory) requests), fetch the new branch above it by number if it is
    /// short enough, check that everything hash-links, and continue from the head.
    async fn compare_by_number(
        &mut self,
        head: Header,
        tip: Seen,
        now: DateTime<Utc>,
    ) -> Result<StepOutcome> {
        let seen = |header: Header| Seen {
            header,
            seen_at: now,
        };
        let changed = |what: String| -> anyhow::Error {
            anyhow!("{what}: the provider's chain changed while being compared")
        };
        let min = self.chain.min().unwrap_or(tip.header.number);
        let top = tip.header.number.min(head.number);

        let top_b = self.canonical(top, head.number).await?;
        let fork = if top_b.hash == self.chain.known(top).hash {
            Some(top)
        } else {
            let min_b = if min == top {
                top_b
            } else {
                self.canonical(min, head.number).await?
            };
            if min_b.hash == self.chain.known(min).hash {
                let (mut lo, mut hi) = (min, top); // lo still matches, hi does not
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if self.canonical(mid, head.number).await?.hash == self.chain.known(mid).hash {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                Some(lo)
            } else if min > 0 && min_b.parent_hash == self.chain.known(min).parent_hash {
                Some(min - 1)
            } else {
                None
            }
        };
        let head_seen = seen(head);

        let Some(fork) = fork else {
            let (from, to) = (min, tip.header.number.max(head.number.saturating_sub(1)));
            store::commit(
                &self.pool,
                &self.provider,
                &[Op::Gap {
                    from,
                    to,
                    reason: "diverged beyond memory: no common block with the provider",
                    head: head_seen,
                }],
            )
            .await?;
            error!(provider = %self.provider, from, to, head = head.number, "tip watch diverged beyond memory");
            self.chain.reset(head_seen);
            return Ok(StepOutcome::Diverged {
                gap_from: from,
                gap_to: to,
            });
        };

        if fork == top {
            // Nothing remembered was replaced: the head is simply far ahead.
            if head.number <= top + 1 {
                return Err(changed(format!(
                    "block {top} is unchanged but the head {} does not follow it",
                    head.number
                )));
            }
            let (from, to) = (top + 1, head.number - 1);
            store::commit(
                &self.pool,
                &self.provider,
                &[Op::Gap {
                    from,
                    to,
                    reason: "tip jumped past --max-walk",
                    head: head_seen,
                }],
            )
            .await?;
            warn!(provider = %self.provider, from, to, "tip watch skipped heights");
            self.chain.reset(head_seen);
            return Ok(StepOutcome::Jump {
                reorg: None,
                gap_from: from,
                gap_to: to,
            });
        }

        // A reorg at `fork`: every remembered block above it was replaced. Fetch
        // the new branch by number (all of it, or only its first block when it
        // is longer than max_walk) and check each block links to the one below.
        let orphaned = self.chain.above(fork);
        let depth = orphaned.len();
        let fork_hash = if fork >= min {
            self.chain.known(fork).hash
        } else {
            self.chain.known(min).parent_hash
        };
        let last = if (top - fork) as usize <= self.opts.max_walk {
            top
        } else {
            fork + 1
        };
        let mut added = Vec::new();
        let mut parent = fork_hash;
        for n in fork + 1..=last {
            let b = self.canonical(n, head.number).await?;
            if b.parent_hash != parent {
                return Err(changed(format!(
                    "block {n} does not link to block {}",
                    n - 1
                )));
            }
            parent = b.hash;
            added.push(seen(b));
        }
        let connects = match head.number {
            h if h < last => return Err(changed(format!("head {h} is below block {last}"))),
            h if h == last => {
                if head.hash != parent {
                    return Err(changed(format!("block {last} is not the head")));
                }
                true
            }
            h if h == last + 1 => {
                if head.parent_hash != parent {
                    return Err(changed(format!(
                        "the head {h} does not link to block {last}"
                    )));
                }
                true
            }
            _ => false,
        };

        let reorg = NewReorg {
            fork_point: fork,
            old_tip: tip.header.number,
            new_tip: head.number,
            orphaned: &orphaned,
            added: &added,
            after_gap: !connects,
        };
        if connects {
            let extend = [head_seen];
            let mut ops = vec![Op::Reorg(reorg)];
            if head.number == last + 1 {
                ops.push(Op::Extend(&extend));
            }
            let ids = store::commit(&self.pool, &self.provider, &ops).await?;
            log_reorg(
                &self.provider,
                ids[0],
                depth,
                fork,
                tip.header.number,
                head.number,
                "block numbers",
            );
            self.chain.replace_above(fork, &added);
            if head.number == last + 1 {
                self.chain.replace_above(last, &extend);
            }
            return Ok(StepOutcome::Reorg {
                id: ids[0],
                depth: depth as u32,
                fork_point: fork,
            });
        }

        let (from, to) = (last + 1, head.number - 1);
        let ids = store::commit(
            &self.pool,
            &self.provider,
            &[
                Op::Reorg(reorg),
                Op::Gap {
                    from,
                    to,
                    reason: "tip jumped past --max-walk",
                    head: head_seen,
                },
            ],
        )
        .await?;
        log_reorg(
            &self.provider,
            ids[0],
            depth,
            fork,
            tip.header.number,
            head.number,
            "block numbers, after a gap",
        );
        warn!(provider = %self.provider, from, to, "tip watch skipped heights");
        self.chain.reset(head_seen);
        Ok(StepOutcome::Jump {
            reorg: Some((ids[0], depth as u32)),
            gap_from: from,
            gap_to: to,
        })
    }

    /// Drops `tipwatch.seen` rows older than the memory window and, with a
    /// retention set, orphan copies older than it. Returns (seen, orphans)
    /// rows deleted.
    pub async fn prune(&self) -> Result<(u64, u64)> {
        let upto = self.chain.tip().and_then(|tip| {
            tip.header
                .number
                .checked_sub(self.opts.memory_blocks as u64)
        });
        let seen = match upto {
            Some(upto) => store::prune_seen(&self.pool, &self.provider, upto).await?,
            None => 0,
        };
        let orphans = match self.opts.orphan_retention_days {
            Some(days) => store::prune_orphans(&self.pool, &self.provider, days).await?,
            None => 0,
        };
        Ok((seen, orphans))
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
                    Ok((seen, orphans)) => {
                        debug!(provider = %self.provider, seen, orphans, "tip watch pruned")
                    }
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
