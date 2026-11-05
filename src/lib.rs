//! SPAVN (`spavn.dev`) — a zero-dependency-bloat terminal coding agent.
//!
//! The crate is split into small single-purpose modules. `main.rs` is a thin binary that
//! wires [`state`] signals to the agent loop; everything reusable lives here so it can be
//! exercised from `tests/` and `cargo test --doc`.
//!
//! Module boundaries:
//!
//! - [`ansi`] — colour constants, display-width math, and the pure dock/input geometry
//!   (no terminal I/O, fully unit-tested).
//! - [`config`] — user config, provider catalog, secrets, i18n.
//! - [`guard`] — command/token safety analysis and access-mode gating.
//! - [`http`] — thin `ureq` wrapper with read-based timeouts.
//! - [`hud`] — terminal primitives, spinner, dock rendering, and input.
//!   Submodules: `keys` (navigation-key decoding), `menus` (interactive pickers with
//!   type-to-filter), `input` (the dock prompt editor).
//! - [`markdown`] — streaming markdown → ANSI renderer.
//! - [`memory`] — project context loading and system-prompt assembly.
//! - [`router`] — endpoint resolution and chat-completion streaming.
//! - [`session`] — session persistence, fork, and compaction plumbing.
//! - [`skills`] — `@skill` discovery, activation, and expansion.
//! - [`slash_commands`] — `/command` dispatch table.
//! - [`spavn_engine`] — agent phases, role dispatch, knowledge artifacts.
//! - [`state`] — the crate's only mutable globals (queues, live tasks, signal flags).
//! - [`tools`] — tool schemas, executors, and approval flow.
//! - [`update`] — version check and the upgrade advice shown to the user.

pub mod ansi;
pub mod config;
pub mod guard;
pub mod http;
pub mod hud;
pub mod markdown;
pub mod memory;
pub mod router;
pub mod session;
pub mod skills;
pub mod slash_commands;
pub mod spavn_engine;
pub mod spill;
pub mod state;
pub mod tools;
pub mod update;

// Re-exported at the crate root so `crate::take_resized()` and friends keep resolving
// from every module without threading a context object through the terminal code.
pub use state::*;
