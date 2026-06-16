#![forbid(unsafe_code)]

//! hb-net — Hoardbook's Nostr relay client (M2).
//!
//! The signaling plane on top of `hb-core`'s offline primitives: it connects to a relay set,
//! publishes the `Event`s `hb-core` builds, fetches them back by `Filter` with **dedup by event
//! id**, resolves a peer's relays via **NIP-65**, mines **NIP-13** proof-of-work, emits **NIP-09**
//! deletions, and wraps/unwraps **NIP-17** gift-wrapped DMs. The oversize-listing **split/restitch**
//! protocol (N4) lives here too. All networking + async lives in this crate so `hb-core` stays a
//! lean, synchronous, exhaustively-unit-tested core.
//!
//! **Contract discipline:** the client publishes exactly the `Event`s `hb-core` builds and parses
//! exactly through `hb-core` parsers — the schema/crypto version discriminants (`hb-v`/`hb-cv`) and
//! the browse-key KDF are never re-implemented here. This crate only adds the transport-layer NIPs
//! (65/17/13/09), dedup, and the listing split.

pub mod client;
pub mod dm;
pub mod error;
pub mod nip09;
pub mod nip65;
pub mod pow;
pub mod split;

pub use client::{dedup_by_id, teaser_search_filter, PublishOutcome, RelayClient};
pub use dm::{unwrap_dm, wrap_dm, DirectMessage};
pub use error::NetError;
pub use nip09::build_deletion;
pub use nip65::{bootstrap_order, build_relay_list, parse_relay_list, RelayList};
pub use pow::{leading_zero_bits, mine_pow, pow_difficulty};
pub use split::{restitch_listing, split_listing, ListingPart};
