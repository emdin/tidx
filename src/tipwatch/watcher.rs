use anyhow::Result;

use super::rpc::{Header, TipRpc};
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
    Extended { blocks: usize, tip: u64 },
    Reorg { id: i64, depth: u32, fork_point: u64 },
    /// The tip moved further than `max_walk`. The old tip was compared with the
    /// provider's block at the same height (`reorg` if it changed) and the
    /// heights in between were recorded as a gap.
    Jump { reorg: Option<(i64, u32)>, gap_from: u64, gap_to: u64 },
    /// No common block within memory: recorded as a gap, chain restarted.
    Diverged { gap_from: u64, gap_to: u64 },
}

pub struct Watcher {
    provider: String,
    rpc: TipRpc,
    pool: Pool,
    opts: WatchOptions,
}

impl Watcher {
    /// Loads the provider's last seen chain from `tipwatch.seen`.
    pub async fn start(provider: &str, rpc: TipRpc, pool: Pool, opts: WatchOptions) -> Result<Self> {
        let _ = (provider, rpc, pool, opts);
        todo!()
    }

    /// One poll: fetch the provider's head, reconcile it with what was seen,
    /// persist the result.
    pub async fn step(&mut self) -> Result<StepOutcome> {
        todo!()
    }

    pub fn tip(&self) -> Option<Header> {
        todo!()
    }

    /// Drops `tipwatch.seen` rows older than the memory window.
    pub async fn prune(&self) -> Result<u64> {
        todo!()
    }

    /// Steps forever, backing off on errors.
    pub async fn run(self) -> Result<()> {
        let _ = (&self.provider, &self.rpc, &self.pool, self.opts);
        todo!()
    }
}
