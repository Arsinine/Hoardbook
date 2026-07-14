// Mirrors hb-core Rust types

export interface IdentityInfo {
	/** The bech32 `npub` — the identity everywhere. */
	npub: string;
	npub_short: string;
	/** The full `hbk…` share code (npub + account browse-key) to hand out. */
	share_code: string;
	/** "os-encrypted" (Windows DPAPI) or "plain-file" (Linux/macOS 0600 file). */
	key_storage: 'os-encrypted' | 'plain-file';
}

export interface SocialLink {
	platform: string; // e.g. "reddit", "discord", "matrix", "github"
	handle: string;
}

export interface Profile {
	display_name: string;
	bio?: string;
	tags: string[];
	since?: number;
	est_size?: string;
	languages: string[];
	contact_hint?: string;
	/** Publicly visible — user explicitly opts in by filling this field. */
	email?: string;
	/** City or region, e.g. "Tokyo" or "EU/Germany". */
	location?: string;
	/** Optional social/contact links. */
	social_links: SocialLink[];
	/** What the user is willing to do: "trade", "seed", "upload", etc. */
	willing_to: string[];
	/** Aggregate content types across all published collections (auto-computed). */
	content_types: string[];
	/** Optional avatar as a `data:` URI (M13 item #13) — never an http(s) URL. */
	picture?: string;
	updated: string; // ISO datetime
}

export interface ReceivedMessage {
	from: string;  // real sender npub (recovered from the NIP-17 seal)
	to: string;    // recipient npub
	content: string;
	sent_at: string; // ISO datetime
}

export interface DirectoryItem {
	name: string;
	item_type: 'File' | 'Folder';
	size?: string;
	format?: string;
	year?: number;
	tags: string[];
	note?: string;
	children: DirectoryItem[];
}

/** Who a collection's listing is sealed to (M10). `Public` = the shared browse-key (anyone with
 *  the share code); `Private` = per-trusted-`npub` gift-wrapped (the browse-key cannot open it).
 *  Matches the Rust `Visibility` serde (PascalCase). */
export type Visibility = 'Public' | 'Private';

export interface Collection {
	slug: string;
	path_alias: string;
	description?: string;
	item_count: number;
	est_size?: string;
	total_bytes: number;
	content_types: string[];
	tags: string[];
	languages: string[];
	/** Public (default) or Private (M10). Absent ⇒ Public (a pre-M10 collection). */
	visibility?: Visibility;
	/** True when the listing is alphabetically sorted. */
	sorted?: boolean;
	last_updated: string;
	listing: DirectoryItem[];
	/** True if this collection has been signed and published to the relay. */
	published?: boolean;
	/** K-of-N part counts when browsed from a peer with a full share code (M13 HANDOVER gap #5).
	 *  Absent for a local draft or a pre-M13 cached peer collection — see
	 *  `browse-view.ts::collectionAvailability` (never render a badge from missing data). */
	parts_total?: number;
	parts_present?: number;
	/** devtest #7 — set when browsing a peer who published only a truncated paywall teaser (collection
	 *  too large to publish whole): the listing carries the first items and `total_items` is the full
	 *  count, so the browser shows the shown items behind a "N more hidden" fade. Absent when whole. */
	truncated?: boolean;
	total_items?: number;
}

/** A trusted peer's decrypted Private collections, grouped by author npub (M10 browse). */
export interface PrivatePeerCollections {
	npub: string;
	collections: Collection[];
}

/** How a contact entered the list (M11). `Manual` = added by hand; `Topic` = auto-added via a shared
 *  §11 Topic (a distinct badge). Absent ⇒ `Manual` (a pre-M11 contact). */
export type ContactSource = 'Manual' | 'Topic';

export interface CachedPeer {
	/** The peer's Nostr identity (bech32 npub) — the stable contact key. */
	npub: string;
	/** How this contact was added — `Manual` or `Topic` (auto-added via a shared Topic). Absent ⇒ Manual. */
	source?: ContactSource;
	/** Hex account browse-key captured from a full `hbk` code (unlocks listings + address). */
	browse_key_hex?: string;
	/** Local impersonation-resistant petname, bound to npub. */
	petname?: string;
	profile?: Profile;
	collections: Collection[];
	online: boolean;
	last_fetched: string;
	local_tags: string[];
	/** §7 word+color impersonation fingerprint, derived from npub by Rust (shape matches
	 *  identity-display.ts::Fingerprint). Absent for a pre-fingerprint stored contact until refreshed. */
	fingerprint?: { words: string[]; colorHex: string };
}

export interface ScanOptions {
	path: string;
	path_alias: string;
	/** Relative, "/"-separated dir paths the user checked in the folder-tree picker. Each is walked
	 *  fully; root-level loose files are always included. (Replaces the old `depth` slider — M8.) */
	include: string[];
	exclude: string[];
}

/** An immediate child directory of a scanned path — one node of the folder-tree picker. */
export interface SubdirEntry {
	name: string;
	/** Absolute path on disk, used to lazily expand this node's own children. */
	path: string;
	/** True if this node has expandable children — a sub-directory or loose files (drives the ▶
	 *  expander). Always false for a file leaf. */
	has_children: boolean;
	/** True for a file leaf, false for a directory (devtest #10 — files are individually selectable). */
	is_file?: boolean;
}

/** Per-collection persisted state. The transfer-era fields (enabled/allowed_paths/speed_cap/
 *  download_limit/require_follow) were removed with the download UI — Hoardbook moves no files
 *  (INV-4). Only the on-disk root survives, used to pre-fill the re-scan dialog. */
export interface ShareSettings {
	root_path?: string;
}

export interface Group {
	name: string;
	pubkeys: string[];
	/** Marks the group trusted (M10): its members receive a sealed copy of every Private
	 *  collection. Absent ⇒ untrusted (a pre-M10 group). */
	trusted?: boolean;
	/** Optional user-chosen colour (CSS hex, e.g. "#ff00aa") for the group chip (M13 W5). Absent ⇒
	 *  no colour (a pre-existing group). */
	color?: string;
}

export interface Watch {
	name: string;
	tags: string[];
	content_types: string[];
	last_fired?: string;
	seen_pubkeys: string[];
}

export interface WatchHit {
	watch_name: string;
	npub: string;
}

// ── Topics (M11; spec §11) ───────────────────────────────────────────────────

/** A Topic I'm a member of (local view). */
export interface TopicView {
	topic_id: string;
	name: string;
	description: string;
	tags: string[];
	private: boolean;
	joined_at: number;
}

/** A discovered public Topic (non-member view) — the roster identities are NOT here (members-only);
 *  the count is a deliberately **spoofable** estimate. */
export interface DiscoveredTopic {
	topic_id: string;
	name: string;
	description: string;
	tags: string[];
	member_count_estimate: number;
}

/** The join-first lookup result (devtest #11) — does this public Topic name already have a room? */
export interface TopicLookup {
	topic_id: string;
	name: string;
	exists: boolean;
	member_count_estimate: number;
}

/** A decrypted 24h channel post. */
export interface ChannelPost {
	author_npub: string;
	body: string;
	ts: number;
}

/** A decrypted member broadcast (M13 Part A app wiring; spec §11/Q1). */
export interface AnnouncementView {
	author_npub: string;
	body: string;
	ts: number;
}

/** The full channel read: posts + announcements, both newest-first (one relay fetch serves both). */
export interface ChannelView {
	posts: ChannelPost[];
	announcements: AnnouncementView[];
}

/** One joined Topic's newest member-broadcast (devtest #2) — the background alert poll's per-topic
 *  row: the Topics nav badge + toast flag it when `latest_ts` is past the seen watermark. */
export interface TopicAnnounceSummary {
	topic_id: string;
	topic_name: string;
	latest_ts: number;
}

// ── Q7 — the stranger-DM Request inbox (M13 Part B) ──────────────────────────

/** A stranger's quarantined Request bucket (message-requests pattern) — seen only when the user
 *  opens the Request pane. Until accepted, no reply is possible. */
export interface DmRequestView {
	npub: string;
	first_seen: number;
	last_message_at: number;
	message_count: number;
	messages: ReceivedMessage[];
	/** §7 word+color impersonation fingerprint, derived from the npub. */
	fingerprint?: { words: string[]; colorHex: string };
}

// (DownloadItem removed — file transfer moved to the Mascara companion.)
