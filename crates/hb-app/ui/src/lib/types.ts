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
	/** True when the listing is alphabetically sorted. */
	sorted?: boolean;
	last_updated: string;
	listing: DirectoryItem[];
	/** True if this collection has been signed and published to the relay. */
	published?: boolean;
}

export interface CachedPeer {
	/** The peer's Nostr identity (bech32 npub) — the stable contact key. */
	npub: string;
	/** Hex account browse-key captured from a full `hbk` code (unlocks listings + address). */
	browse_key_hex?: string;
	/** Local impersonation-resistant petname, bound to npub. */
	petname?: string;
	profile?: Profile;
	collections: Collection[];
	online: boolean;
	last_fetched: string;
	local_tags: string[];
}

export interface ScanOptions {
	path: string;
	path_alias: string;
	depth: number;
	exclude: string[];
}

export interface ShareSettings {
	enabled: boolean;
	root_path?: string;
	allowed_paths: string[];
	speed_cap_kbps?: number;
	download_limit?: number;
	require_follow: boolean;
}

export interface Group {
	name: string;
	pubkeys: string[];
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

// (DownloadItem removed — file transfer moved to the Mascara companion.)
