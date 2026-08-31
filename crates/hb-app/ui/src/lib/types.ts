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
	/** M16 W4 — the full-tree snapshot fingerprint (both the teaser and the full manifest carry it);
	 *  passed to `import_manifest` to gate an imported manifest for staleness. Absent when unmarked. */
	snapshot_fingerprint?: string;
	/** M16 W4 — unix secs of a `.hbmanifest` imported to upgrade this truncated teaser to the full
	 *  tree. Set only after an import; the UI tags "Full manifest imported · <date>" and the fade lifts. */
	manifest_imported_at?: number;
	/** M16 W4 — the id of the teaser (index) event this collection was browsed from, so an "ask the
	 *  owner for the full list" DM can name the exact event. Absent for a cached/pre-M16 collection. */
	teaser_event_id?: string;
}

/** QURATOR-79 carrier 4 — the provenance half of an imported manifest's result. The base shape
 *  (`slug`, `collection`, `created_at`, `stale`) lives in `api.ts::ImportedManifest`; this is the
 *  additive-optional provenance mirror of the Rust `browse.rs::ImportedManifest` fields, absent on
 *  every pre-carrier-4 serve. `servedBy` is the serving peer's npub (None ⇒ the author served it
 *  directly); `cachedAt` is when that peer's cached copy was taken. The envelope's own clock is
 *  `collection.manifest_imported_at` — there is no second one here. */
export interface ImportedManifestProvenance {
	served_by?: string;
	cached_at?: number;
}

/** A trusted peer's decrypted Private collections, grouped by author npub (M10 browse). */
export interface PrivatePeerCollections {
	npub: string;
	collections: Collection[];
}

/** How a contact entered the list (M11). `Manual` = added by hand; `Topic` = auto-added via a shared
 *  §11 Topic (a distinct badge). Absent ⇒ `Manual` (a pre-M11 contact). */
export type ContactSource = 'Manual' | 'Topic';

/** QURATOR-134 — the tri-state a keyless contact's listings resolve to (see
 *  `CachedPeer.listings_state`). Mirrors hb-app's `store::ListingsStatus`, which mirrors
 *  hb-net's `ListingsState` — the one implementation; the UI renders, never re-derives. */
export type ListingsStatus = 'Fetched' | 'Sealed' | 'FetchFailed';

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
	/** QURATOR-134 — WHY `collections` looks the way it does for a KEYLESS contact, computed by
	 *  the hb-app browse command from hb-net's enumeration (never re-derived here from
	 *  `collections.length === 0`, which cannot tell "published nothing" from "sealed"):
	 *  'Fetched' = enumeration completed, peer authored no listing events (honest empty);
	 *  'Sealed' = listings exist but none decryptable (the genuine 🔒 locked case);
	 *  'FetchFailed' = the enumeration itself failed (error + Retry). Absent ⇒ 'Fetched'
	 *  (a pre-QURATOR-134 cached contact; the least-wrong reading). */
	listings_state?: ListingsStatus;
	online: boolean;
	/** When WE last polled — our cache age. Rendered as "checked {t}", never "seen {t}" (M17 W5.1:
	 *  the old label reported our poll and so claimed "just now" about a peer gone for a week). */
	last_fetched: string;
	/** When we last saw THEIR presence beacon (RFC3339), stamped by the online poll and
	 *  persisted so the age survives a restart. Absent ⇒ never observed → "Last seen — unknown". */
	last_presence?: string;
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
 *  the count is a deliberately **spoofable** estimate. `member_count_estimate` is `null` on the W1
 *  paint path (QURATOR-143): the count has not been fetched yet, because ranking is lazy — the
 *  sidebar ORDERS by the count once `topicRank` lands it, but never displays it, and `null` must
 *  never render as "0 claimed". */
export interface DiscoveredTopic {
	topic_id: string;
	name: string;
	description: string;
	tags: string[];
	member_count_estimate: number | null;
}

/** One lazy-ranking result (QURATOR-143 W1): a `topic_id` + its spoofable count, count-desc. */
export interface TopicRank {
	topic_id: string;
	member_count_estimate: number;
}

/** The join-first lookup result (devtest #11) — does this public Topic name already have a room? */
export interface TopicLookup {
	topic_id: string;
	name: string;
	exists: boolean;
	member_count_estimate: number;
}

/** A side-effect-free preview of a pending private-Topic invite (W8 consent gate) — the UI shows who
 *  is vouching (`issuer_npub`) + the topic name BEFORE committing the redeem. */
export interface TopicInvitePreview {
	topic_id: string;
	name: string;
	description: string;
	/** The invite ISSUER's npub (bech32) — whose key sealed the invite = who is vouching for the join. */
	issuer_npub: string;
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

// (DownloadItem removed in v0.9.6. Still gone after M18: the transport plane carries manifests, not
// collection files (INV-4′), so there is no download item to model.)
