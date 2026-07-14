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

pub mod browse;
pub mod cache;
pub mod client;
pub mod count;
pub mod discover;
pub mod dm;
pub mod error;
pub mod nip09;
pub mod nip65;
pub mod pow;
pub mod priv_browse;
pub mod render;
pub mod split;
pub mod topic;

pub use browse::{
    browse_peer_listings, browse_share_code, parse_share_code, publish_listing,
    publish_listing_capped, resolve_peer_relays, resolve_recipient_relays, search_teasers,
    BrowseResult, PublishedListing,
};
pub use cache::{cache_decision, CacheDecision, CachedListing, CACHE_FRESH_SECS};
pub use client::{
    dedup_by_id, pool_is_live, teaser_search_filter, PublishOutcome, RelayClient, RelayHealth,
};
pub use count::{count_online, count_userbase, presence_count_filter, userbase_filter};
pub use discover::{ingest_teasers, select_newest_by_created_at, teaser_matches, SearchHit};
pub use dm::{unwrap_dm, wrap_dm, DirectMessage};
pub use error::NetError;
pub use nip09::build_deletion;
pub use nip65::{bootstrap_order, build_relay_list, inbox_order, parse_relay_list, RelayList};
pub use priv_browse::{dedup_newest, fetch_private_listings, publish_private_listing};
pub use pow::{leading_zero_bits, mine_pow, pow_difficulty};
pub use render::{render_listing, RenderedListing, MAX_LISTING_PARTS};
pub use split::{restitch_listing, split_listing, truncate_listing, ListingPart, TruncatedListing};
pub use topic::{
    announce_to_topic, approve_join, discover_public_topics, fetch_announce, fetch_channel,
    fetch_channel_full, fetch_invite, fetch_join_requests, fetch_membership_events, fetch_roster,
    join_public, join_request_message, join_topic, leave_topic, member_count, parse_join_request,
    post_to_channel, publish_topic, request_join, ChannelRead, JoinRequest, INVITE_TTL_SECS,
    TOPIC_DISCOVERY_CAP,
};
