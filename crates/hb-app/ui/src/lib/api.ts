import { invoke as _invoke } from '@tauri-apps/api/core';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	if (!isTauri) return Promise.reject(new Error(`Tauri not available (cmd: ${cmd})`));
	return _invoke<T>(cmd, args);
}
import type {
	CachedPeer,
	Collection,
	Group,
	IdentityInfo,
	Profile,
	ReceivedMessage,
	PrivatePeerCollections,
	ScanOptions,
	ShareSettings,
	SubdirEntry,
	Visibility,
	Watch,
	WatchHit,
	TopicView,
	DiscoveredTopic,
	TopicLookup,
	ChannelPost,
	ChannelView,
	TopicAnnounceSummary,
	DmRequestView,
} from './types.js';

// ── Identity ─────────────────────────────────────────────────────────────────

export const generateKeypair = () => invoke<IdentityInfo>('generate_keypair');

export const getIdentity = () => invoke<IdentityInfo | null>('get_identity');

/** The full `hbk…` share code to hand out. */
export const getShareCode = () => invoke<string>('get_share_code');

export const validateShareCode = (code: string) =>
	invoke<boolean>('validate_share_code', { code });

/** Import an existing Nostr secret key (`nsec`/hex). The UI must show the linking-privacy warning
 *  first (there is no offline oracle to detect a public/Qurator npub). */
export const importNsec = (nsec: string) => invoke<IdentityInfo>('import_nsec', { nsec });

/** Export a portable whole-`~/.hoardbook` backup to `path`. `passphrase = null` is the plaintext
 *  export (behind a blunt warning); a passphrase encrypts with Argon2id → XChaCha20-Poly1305. */
export const backupData = (passphrase: string | null, path: string) =>
	invoke<void>('backup_data', { passphrase, path });

/** Does the backup at `path` need a passphrase to restore? (cheap header peek, no KDF) */
export const peekBackup = (path: string) => invoke<boolean>('peek_backup', { path });

/** Restore a whole-directory backup, re-wrapping secrets at rest. The target must be empty (wipe
 *  first); `passphrase = null` works for a plaintext archive. */
export const restoreData = (passphrase: string | null, path: string) =>
	invoke<IdentityInfo>('restore_data', { passphrase, path });

export const wipeData = () => invoke<void>('wipe_data');

// ── Profile ───────────────────────────────────────────────────────────────────

export const saveProfile = (profile: Profile) => invoke<void>('save_profile', { profile });

// Backend serde may omit empty Vec fields (skip_serializing_if). Coerce them
// back to [] so frontend code can call .find/.map without crashing.
function normalizeProfile(p: Profile | null): Profile | null {
	if (!p) return p;
	return {
		...p,
		tags: p.tags ?? [],
		languages: p.languages ?? [],
		social_links: p.social_links ?? [],
	};
}

export const getProfile = () => invoke<Profile | null>('get_profile').then(normalizeProfile);

export const publishProfile = () => invoke<void>('publish_profile');

export const unpublishProfile = () => invoke<void>('unpublish_profile');

export const hasPublishedProfile = () => invoke<boolean>('has_published_profile');

// ── Collections ───────────────────────────────────────────────────────────────

export const scanDirectory = (opts: ScanOptions) =>
	invoke<Collection>('scan_directory', { opts });

/** Immediate child directories of `path`, for lazy folder-tree expansion (M8). */
export const listSubdirs = (path: string) =>
	invoke<SubdirEntry[]>('list_subdirs', { path });

export const getCollections = () => invoke<Collection[]>('get_collections');

export const deleteCollection = (slug: string) => invoke<void>('delete_collection', { slug });

/** The outcome of publishing a Public collection (devtest #7) — `truncated` when the listing was too
 *  large and only a paywall teaser (`shown_items` of `total_items`) was published. */
export interface PublishSummary {
	truncated: boolean;
	shown_items: number;
	total_items: number;
}

export const publishCollection = (slug: string) =>
	invoke<PublishSummary>('publish_collection', { slug });

/** Unpublish a collection (spec §4): NIP-09-deletes its listing events (Public only — a Private
 *  collection's gift-wrapped events are ephemeral-keyed and cannot be deleted by this identity),
 *  drops the local published marker (stops auto-republish), and refreshes the profile teaser. */
export const unpublishCollection = (slug: string) =>
	invoke<void>('unpublish_collection', { slug });

export const updateCollectionMeta = (slug: string, description: string | undefined, contentTypes: string[], tags: string[], languages: string[], sorted: boolean) =>
	invoke<void>('update_collection_meta', { slug, description, contentTypes, tags, languages, sorted });

/** Set a collection's visibility (M10). Public = browse-key; Private = per-trusted-npub sealed. */
export const updateCollectionVisibility = (slug: string, visibility: Visibility) =>
	invoke<void>('update_collection_visibility', { slug, visibility });

/** Fetch + decrypt the Private collections trusted peers have sealed to me, grouped by author. */
export const browsePrivateCollections = () =>
	invoke<PrivatePeerCollections[]>('browse_private_collections');

export const exportCollection = (slug: string, format: 'text' | 'markdown') =>
	invoke<string>('export_collection', { slug, format });

/** M16 W4: serialize a collection's full-listing manifest envelope to a user-picked `.hbmanifest`
 *  file. `path` comes from the save dialog; Hoardbook writes the file and moves no bytes (INV-4). */
export const exportManifest = (slug: string, path: string) =>
	invoke<void>('export_manifest', { slug, path });

// ── Settings ──────────────────────────────────────────────────────────────────

export interface Settings {
	relay_urls: string[];
	allow_dms: boolean;
	/** The one-time pre-first-download IP-exposure notice has been acknowledged. */
	privacy_notice_acknowledged: boolean;
	/** App version last seen running — drives the visible-after "now on vX.Y" notice. */
	last_seen_version: string;
	/** M9: auto-update a published listing when its source tree changes (filesystem-watch). */
	snapshot_auto_update: boolean;
	/** M9: opt-in low-frequency reconcile poll for shares edited from another host (SMB). */
	snapshot_reconcile_poll: boolean;
	/** M9: show the optional "🟢 N online" indicator (relay-derived; no telemetry). */
	show_online_count: boolean;
	/** devtest #5: opt into tag/content-type discoverability. Default **false** — off means people
	 *  can't find you by tag or content-type search; they can still reach you with your npub or
	 *  share code, and your contacts are unaffected. */
	discoverable: boolean;
	/** M16 W3: the owner's dedicated higher-capacity **big relay** for collections too large to
	 *  publish whole. When set, publishing a large collection also sends its full listing here (only),
	 *  so a browser with the share code can fetch the rest. **Empty = off** (only the preview teaser
	 *  is published — today's behaviour). */
	big_relay_url: string;
}

export const getSettings = () => invoke<Settings>('get_settings');

export const saveSettings = (settings: Settings) => invoke<void>('save_settings', { settings });

export const checkRelay = (url: string) => invoke<void>('check_relay', { url });

/** Live per-relay reachability on the data path (M12 W1, Decision D) — so a "–"/Offline read can
 *  say *why*. One entry per **configured** relay. */
export interface RelayHealth {
	url: string;
	/** Lowercase status label: `connected` / `connecting` / `disconnected` / … */
	status: string;
	connected: boolean;
	lastError: string | null;
}

/** Live status of the persistent shared client's configured relays. Best-effort. */
export const relayStatus = () => invoke<RelayHealth[]>('relay_status');

/** Per-relay outcome of the most recent presence-beacon publish (devtest #9 same-NAT diagnosis) —
 *  the beacon rides the same write path as every outbound publish (DMs/discovery), so a per-relay
 *  reject here is evidence for those too, not presence-only. */
export interface BeaconRelayOutcome {
	url: string;
	/** `"accepted"` or `"rejected"`. */
	outcome: string;
	reason: string | null;
}

/** Rolling beacon-health snapshot. */
export interface BeaconReport {
	/** Unix seconds of the most recent attempt (0 = never attempted). */
	lastAttemptAt: number;
	/** Unix seconds of the most recent attempt that reached a relay. */
	lastSuccessAt: number;
	relays: BeaconRelayOutcome[];
	lastError: string | null;
}

/** Live beacon-publish health. Best-effort. */
export const beaconStatus = () => invoke<BeaconReport>('beacon_status');

/** Record that the one-time pre-first-download IP-exposure notice was acknowledged. */
export const acknowledgePrivacyNotice = () => invoke<void>('acknowledge_privacy_notice');

// ── Network stats (M9 — relay-derived count, no telemetry) ──────────────────────

/**
 * The "🟢 N online" chip's data. `online` is `null` when the count is unknown (no cache yet and no
 * reachable relay) — render "–" / hide, never a misleading "0". An estimate per relay-set.
 */
export interface OnlineCount {
	online: number | null;
	fetched_at: string | null;
	relay_set: string[];
}

/** Best-effort, cached online count (relay-derived). Returns immediately; refreshes in the background. */
export const onlineCount = () => invoke<OnlineCount>('online_count');

// ── Browse / Contacts ─────────────────────────────────────────────────────────

/** `code` is a pasted share code (bare npub or full `hbk…`). */
export const pasteKey = (code: string) => invoke<CachedPeer>('paste_key', { code });

/** `petname` is the M13 W5 seam — an optional user-supplied nickname set at follow-time, overriding
 *  the auto-derived one. Pass undefined/omit to keep the auto-derived petname. */
export const follow = (code: string, groupName?: string, petname?: string) =>
	invoke<void>('follow', { code, groupName: groupName ?? null, petname: petname ?? null });

export const getContacts = () => invoke<CachedPeer[]>('get_contacts');

export const unfollowContact = (npub: string) => invoke<void>('unfollow_contact', { npub });

export const refreshContact = (npub: string) => invoke<CachedPeer>('refresh_contact', { npub });

/** M16 W4 — the result of importing a `.hbmanifest`: the full-tree collection (fade lifted), and
 *  `stale` when the manifest predates the teaser the browser is showing (imported anyway, with a warn). */
export interface ImportedManifest {
	slug: string;
	collection: Collection;
	created_at: number;
	stale: boolean;
}

/** M16 W4 — import a full-listing manifest the user received (a picked file path OR pasted text/base64),
 *  upgrading a truncated teaser to the whole tree. The backend pins the manifest author to `npub` and
 *  verifies the signature before decrypting; `newestFingerprint` (the teaser's) drives the stale warn. */
export const importManifest = (
	npub: string,
	expectedSlug: string,
	source: { path?: string; pasted?: string },
	newestFingerprint?: string,
) =>
	invoke<ImportedManifest>('import_manifest', {
		npub,
		expectedSlug,
		path: source.path ?? null,
		pasted: source.pasted ?? null,
		newestFingerprint: newestFingerprint ?? null,
	});

export const setContactTags = (npub: string, tags: string[]) =>
	invoke<void>('set_contact_tags', { npub, tags });

/** Set a contact's local, user-editable petname (M13 W5) — bound to the npub, never shared. */
export const setContactPetname = (npub: string, petname: string) =>
	invoke<void>('set_contact_petname', { npub, petname });

// ── Discovery (§6) — M12 W3 ─────────────────────────────────────────────────────

/** A §6 Discovery teaser card. Carries only the opt-in public teaser + the §7 fingerprint — never a
 *  listing or browse-key (DISC3): a hit surfaces the advertisement, not the hoard. */
export interface PeerSearchHit {
	npub: string;
	display_name: string;
	bio: string | null;
	tags: string[];
	content_types: string[];
	picture: string | null;
	fingerprint: { words: string[]; colorHex: string } | null;
}

/** Search public teasers by tag (AND) / content-type (OR). ≥1 filter is required (the backend
 *  rejects an empty search — no unfiltered global peer list). */
export const searchPeers = (tags: string[], contentTypes: string[]) =>
	invoke<PeerSearchHit[]>('search_peers', { tags, contentTypes });

// ── Collection root path ────────────────────────────────────────────────────────

/** The persisted on-disk root of a collection (used to pre-fill the re-scan dialog). */
export const getShareSettings = (slug: string) =>
	invoke<ShareSettings>('get_share_settings', { slug });


// ── Chat ──────────────────────────────────────────────────────────────────────

export const sendMessage = (to: string, content: string) =>
	invoke<ReceivedMessage>('send_message', { to, content });

/** M16 W4 — DM the owner a structured request for a truncated collection's full manifest (the blessed
 *  "ask by DM" seam). One relay write; the owner decides whether to export + ticket it (no auto-produce). */
export const requestManifest = (
	npub: string,
	slug: string,
	fingerprintSeen: string,
	teaserEventId?: string,
	mascaraPubkey?: string,
) =>
	invoke<void>('request_manifest', {
		npub,
		slug,
		fingerprintSeen,
		teaserEventId: teaserEventId ?? null,
		mascaraPubkey: mascaraPubkey ?? null,
	});

export const getMessages = () => invoke<ReceivedMessage[]>('get_messages');

// ── Unified read state (devtest #16) ────────────────────────────────────────────

/** The per-peer last-read watermark (npub → RFC3339 timestamp of the newest message seen in that
 *  conversation) — a pure local read, no relay I/O. The single source the unread badge derives from. */
export const getReadState = () => invoke<Record<string, string>>('get_read_state');

/** Advance `npub`'s read watermark to `sentAt` (never rewinds — see the Rust `advance_read_watermark`). */
export const advanceReadWatermark = (npub: string, sentAt: string) =>
	invoke<void>('advance_read_watermark', { npub, sentAt });

// ── Q7 — the stranger-DM Request inbox (M13 Part B) ──────────────────────────

/** List the quarantined Request buckets — a pure local read, no relay I/O. */
export const dmRequests = () => invoke<DmRequestView[]>('dm_requests');

/** Accept a stranger's Request bucket: adds them as a contact (no browse-key) and returns the
 *  drained messages to seed straight into the conversation. `petname` is the W5 seam (pass null for
 *  now — the petname-on-accept dialog is a follow-up UI workstream). */
export const dmRequestAccept = (npub: string, petname?: string | null) =>
	invoke<ReceivedMessage[]>('dm_request_accept', { npub, petname: petname ?? null });

/** Decline a Request bucket — remembered permanently until the sender becomes a contact normally. */
export const dmRequestDecline = (npub: string) => invoke<void>('dm_request_decline', { npub });

/** Block a sender: deletes any Request bucket/decline record and adds them to the local blocklist. */
export const dmBlock = (npub: string) => invoke<void>('dm_block', { npub });

export const dmUnblock = (npub: string) => invoke<void>('dm_unblock', { npub });

export const dmBlockedList = () => invoke<string[]>('dm_blocked_list');

// ── Updates ───────────────────────────────────────────────────────────────────

export interface UpdateInfo { version: string; body?: string; }
export interface UpdateNotice { version: string; }
export const checkUpdate   = () => invoke<UpdateInfo | null>('check_update');
/** Background download + minisign-verify, staged for deferred install (Obsidian pattern). Returns
 *  the staged version, or null if up to date. Does NOT restart. */
export const downloadUpdate = () => invoke<string | null>('download_update');
/** Apply a staged update now and relaunch (explicit user action). */
export const applyStagedUpdate = () => invoke<void>('apply_staged_update');
/** The once-per-version "now running vX.Y" notice (visible-after); returns null if no version change. */
export const takeUpdateNotice = () => invoke<UpdateNotice | null>('take_update_notice');

// ── Groups ────────────────────────────────────────────────────────────────────

export const groupsGet    = () => invoke<Group[]>('groups_get');
/** `color` (M13 W5) is an optional CSS hex string for the group chip. */
export const groupsCreate = (name: string, color?: string) =>
	invoke<Group>('groups_create', { name, color: color ?? null });
export const groupsRename = (oldName: string, newName: string) =>
	invoke<void>('groups_rename', { oldName, newName });
export const groupsDelete   = (name: string) => invoke<void>('groups_delete', { name });
export const groupsAssign   = (npub: string, groupName: string) =>
	invoke<void>('groups_assign', { npub, groupName });
export const groupsUnassign = (npub: string, groupName: string) =>
	invoke<void>('groups_unassign', { npub, groupName });

/** Mark a contact group trusted/untrusted for Private collections (M10). */
export const groupsSetTrusted = (name: string, trusted: boolean) =>
	invoke<void>('groups_set_trusted', { name, trusted });

/** Atomically replace all group memberships for a contact. Pass [] for Ungrouped. */
export const contactUpdateGroups = (npub: string, groupNames: string[]) =>
	invoke<void>('contact_update_groups', { npub, groupNames });

// ── Watches ───────────────────────────────────────────────────────────────────

export const watchesGet    = () => invoke<Watch[]>('watches_get');
export const watchesCreate = (name: string, tags: string[], contentTypes: string[]) =>
	invoke<Watch>('watches_create', { name, tags, contentTypes });
export const watchesDelete   = (name: string) => invoke<void>('watches_delete', { name });
export const watchesEvaluate = (candidates: string[]) =>
	invoke<WatchHit[]>('watches_evaluate', { candidates });

// ── Topics (M11; spec §11) ─────────────────────────────────────────────────────

export const topicList = () => invoke<TopicView[]>('topic_list');

export const topicCreate = (
	name: string,
	description: string,
	tags: string[],
	isPrivate: boolean,
) => invoke<TopicView>('topic_create', { name, description, tags, private: isPrivate });

export const topicDiscover = (tags: string[]) =>
	invoke<DiscoveredTopic[]>('topic_discover', { tags });

/** Join-first lookup (devtest #11): does this public Topic name already have a room? Never call for
 *  a private Topic (no announce to find). */
export const topicLookup = (name: string) => invoke<TopicLookup>('topic_lookup', { name });

export const topicJoinPublic = (name: string) =>
	invoke<TopicView>('topic_join_public', { name });

/** Redeem a private-Topic invite addressed to me (returns the joined Topic, or null if none found). */
export const topicRedeemInvite = () => invoke<TopicView | null>('topic_redeem_invite');

export const topicRequestJoin = (memberNpub: string, topicId: string, name: string) =>
	invoke<void>('topic_request_join', { memberNpub, topicId, name });

/** Invite/admit a peer into a Topic I'm in (any member may invite — M3). */
export const topicInvite = (topicId: string, inviteeNpub: string) =>
	invoke<void>('topic_invite', { topicId, inviteeNpub });

export const topicLeave = (topicId: string) => invoke<void>('topic_leave', { topicId });

export const topicRoster = (topicId: string) => invoke<string[]>('topic_roster', { topicId });

/** The 24h channel: posts + announcements, both newest-first (M13 Part A app wiring). */
export const topicChannel = (topicId: string) =>
	invoke<ChannelView>('topic_channel', { topicId });

export const topicPost = (topicId: string, body: string) =>
	invoke<void>('topic_post', { topicId, body });

/** Broadcast an announce to a Topic's channel — rate-limited to one per topic per 60 min (Q1). */
export const topicAnnounce = (topicId: string, body: string) =>
	invoke<void>('topic_announce', { topicId, body });

/** Remaining announce cooldown for `topicId`, in seconds (0 = ready) — drives the button state. */
export const topicAnnounceStatus = (topicId: string) =>
	invoke<number>('topic_announce_status', { topicId });

/** devtest #2 — newest announcement per joined Topic, for the nav-badge/toast alert poll. Reads only. */
export const topicAnnouncements = () =>
	invoke<TopicAnnounceSummary[]>('topic_announcements');

/** devtest #2 — persisted per-topic announcement-seen watermarks (topic_id → newest seen ts). */
export const topicAnnounceSeen = () =>
	invoke<Record<string, number>>('topic_announce_seen');

/** devtest #2 — mark a Topic's announcements read up to `ts` (advances the watermark, never rewinds). */
export const topicAnnounceMarkSeen = (topicId: string, ts: number) =>
	invoke<void>('topic_announce_mark_seen', { topicId, ts });
