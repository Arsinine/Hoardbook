//! M0 foundation spike — see Cargo.toml for the four legs this de-risks.
//!
//! This crate is throwaway. Its job is a go/no-go answer on the four v0.9 unknowns
//! before the M1 `hb-core` rewrite commits to them. Each leg is a module with a
//! `demo()` (human-readable proof, used by the binary) and `#[cfg(test)]` assertions
//! (the real gate — run with `cargo test -p hb-m0-spike`).

pub mod binding;
pub mod identity;
pub mod listing;
pub mod relay;

/// Provisional Nostr event kinds for Hoardbook.
///
/// NOT registered — Nostr kind registration is an open spec question
/// (`HOARDBOOK_SPEC.md` → Open Questions; `HANDOVER.md` §B). The M1 rewrite must
/// lock these. Chosen to land in the correct NIP-01 behavioural ranges:
///   - presence is **replaceable** (10000–19999): one-per-author, newest wins.
///   - listing is **addressable/parameterized-replaceable** (30000–39999): one per
///     `d`=slug, so re-snapshotting a collection replaces its prior event.
pub const KIND_PRESENCE: u16 = 11_111;
pub const KIND_LISTING: u16 = 31_111;
