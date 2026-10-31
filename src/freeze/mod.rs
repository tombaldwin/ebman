//! Cross-process fleet freeze (0.28, folded from the MCP v3 notes).
//! A TUI's `:freeze-deploys` / `:incident START`, and `ebman freeze`,
//! persist a marker so every process refuses to dispatch while one
//! holds: the TUI, the MCP write tools, and the CLI write paths
//! (`ebman action`, `audit replay`, `lint --fix`).
//!
//! One file per holder, under `freeze.d/` beside the marker path:
//! `persistent.json` for `ebman freeze`, which holds until an explicit
//! thaw, and `session-<pid>.json` for each TUI session, which holds
//! while that process lives. A single shared file was last-writer-wins:
//! a TUI's `:incident START` replaced an `ebman freeze`, and its
//! `:incident END` or exit then deleted it, lifting a freeze nobody had
//! thawed; and `ebman freeze` replaced a live session's marker, so a
//! later `ebman thaw` lifted the session's freeze too.
//!
//! Session scope is kept by the pid: a session file whose process is
//! gone (or whose pid was reused) is stale, and the next reader removes
//! it and records that in the ended-freeze file, so the next person to
//! look learns a freeze ended with its session rather than by decision.
//! Demo-mode TUIs never persist.
//!
//! The legacy `freeze.json` is kept as a mirror of one live holder for
//! ebman older than 0.47, which reads only that file, and every read
//! puts it right. A mirror is marked so; one without the mark was
//! written by an older TUI and is a holder in its own right. Everything
//! goes through a [`FreezeStore`] the surface holds: a TUI's `App`, an
//! MCP `Server`, a CLI write's `WriteEnv`. Deleting `freeze.d/` and `freeze.json` is
//! the manual unfreeze of last resort. See
//! `docs/design/persistent-freeze.md`.

use std::path::{Path, PathBuf};

mod liveness;
mod marker;
mod policy;
mod record;
mod store;
#[cfg(test)]
mod tests;

pub(crate) use liveness::*;
pub(crate) use marker::*;
pub(crate) use policy::*;
pub(crate) use record::*;
pub(crate) use store::*;
