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
	ChannelPost,
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

export const publishCollection = (slug: string) =>
	invoke<void>('publish_collection', { slug });

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
}

export const getSettings = () => invoke<Settings>('get_settings');

export const saveSettings = (settings: Settings) => invoke<void>('save_settings', { settings });

export const checkRelay = (url: string) => invoke<void>('check_relay', { url });

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

export const follow = (code: string, groupName?: string) =>
	invoke<void>('follow', { code, groupName: groupName ?? null });

export const getContacts = () => invoke<CachedPeer[]>('get_contacts');

export const unfollowContact = (npub: string) => invoke<void>('unfollow_contact', { npub });

export const refreshContact = (npub: string) => invoke<CachedPeer>('refresh_contact', { npub });

export const setContactTags = (npub: string, tags: string[]) =>
	invoke<void>('set_contact_tags', { npub, tags });

// ── Sharing ───────────────────────────────────────────────────────────────────

export const getShareSettings = (slug: string) =>
	invoke<ShareSettings>('get_share_settings', { slug });

export const saveShareSettings = (slug: string, settings: ShareSettings) =>
	invoke<void>('save_share_settings', { slug, settings });


// ── Chat ──────────────────────────────────────────────────────────────────────

export const sendMessage = (to: string, content: string) =>
	invoke<ReceivedMessage>('send_message', { to, content });

export const getMessages = () => invoke<ReceivedMessage[]>('get_messages');

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
export const groupsCreate = (name: string) => invoke<Group>('groups_create', { name });
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

export const topicChannel = (topicId: string) =>
	invoke<ChannelPost[]>('topic_channel', { topicId });

export const topicPost = (topicId: string, body: string) =>
	invoke<void>('topic_post', { topicId, body });
