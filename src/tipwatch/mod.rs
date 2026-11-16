//! `tidx watch-tip`: records every reorg an RPC provider makes at the chain
//! tip, at any depth.
//!
//! The engine indexes `head_delay_blocks` behind the tip, so the reorg archive
//! only sees reorgs deeper than that window. Here each provider is followed on
//! its own: a reorg is a provider replacing blocks *it* reported earlier, so two
//! providers that disagree never look like a reorg.

pub mod rpc;
pub mod store;
pub mod watcher;
pub mod ws;

pub use rpc::{Header, TipRpc};
pub use watcher::{StepOutcome, WatchOptions, Watcher};
