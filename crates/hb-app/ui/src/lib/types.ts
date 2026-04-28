// Mirrors hb-core Rust types

export interface IdentityInfo {
	hb_id: string;
	hb_id_short: string;
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
	updated: string; // ISO datetime
}

export interface ReceivedMessage {
	from: string;  // sender hb_id
	to: string;    // recipient hb_id
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
	content_type: string[];
	last_updated: string;
	listing: DirectoryItem[];
	/** True if this collection has been signed and published to the relay. */
	published?: boolean;
}

export interface CachedPeer {
	hb_id: string;
	profile?: Profile;
	collections: Collection[];
	online: boolean;
	node_addr?: string;
	last_fetched: string;
	local_tags: string[];
}

export interface ScanOptions {
	path: string;
	path_alias: string;
	depth: number;
	exclude: string[];
}

export interface DirectoryPeer {
	hb_id: string;
	profile?: Profile;
}

export interface ReceivedChannelMessage {
	from: string;
	content: string;
	sent_at: string; // ISO datetime
}

export interface ShareSettings {
	enabled: boolean;
	root_path?: string;
	allowed_paths: string[];
	speed_cap_kbps?: number;
	download_limit?: number;
	require_follow: boolean;
}
