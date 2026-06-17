import { invoke as _invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

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
	ScanOptions,
	ShareSettings,
	Watch,
	WatchHit,
} from './types.js';

// ── Identity ─────────────────────────────────────────────────────────────────

export const generateKeypair = () => invoke<IdentityInfo>('generate_keypair');

export const getIdentity = () => invoke<IdentityInfo | null>('get_identity');

/** The full `hbk…` share code to hand out. */
export const getShareCode = () => invoke<string>('get_share_code');

export const validateShareCode = (code: string) =>
	invoke<boolean>('validate_share_code', { code });

export const exportKeypair = () => invoke<string>('export_keypair');

export const saveKeypairFile = (path: string) => invoke<void>('save_keypair_file', { path });

export const importKeypair = (path: string) => invoke<IdentityInfo>('import_keypair', { path });

export const getNodeAddr = () => invoke<string | null>('get_node_addr');

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

export const getCollections = () => invoke<Collection[]>('get_collections');

export const deleteCollection = (slug: string) => invoke<void>('delete_collection', { slug });

export const publishCollection = (slug: string) =>
	invoke<void>('publish_collection', { slug });

export const updateCollectionMeta = (slug: string, description: string | undefined, contentTypes: string[], tags: string[], languages: string[], sorted: boolean) =>
	invoke<void>('update_collection_meta', { slug, description, contentTypes, tags, languages, sorted });

export const exportCollection = (slug: string, format: 'text' | 'markdown') =>
	invoke<string>('export_collection', { slug, format });

// ── Settings ──────────────────────────────────────────────────────────────────

export interface Settings {
	relay_urls: string[];
	allow_dms: boolean;
}

export const getSettings = () => invoke<Settings>('get_settings');

export const saveSettings = (settings: Settings) => invoke<void>('save_settings', { settings });

export const checkRelay = (url: string) => invoke<void>('check_relay', { url });

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

export const requestDownload = (
	peer: string,
	slug: string,
	path: string,
	save_path: string,
	expected_sha256?: string,
) => invoke<number>('request_download', { peer, slug, path, savePath: save_path, expectedSha256: expected_sha256 ?? null });

export const cancelDownload = (id: number) =>
	invoke<boolean>('cancel_download', { downloadId: id });

interface DownloadProgressPayload {
	id: number;
	filename: string;
	bytes_done: number;
	bytes_total: number;
	bytes_per_sec: number;
	status: 'active' | 'done' | 'error' | 'cancelled';
	error?: string;
}

export async function listenDownloadProgress(
	cb: (ev: DownloadProgressPayload) => void,
): Promise<() => void> {
	if (!isTauri) return () => {};
	return listen<DownloadProgressPayload>('download:progress', ({ payload }) => cb(payload));
}

// ── Chat ──────────────────────────────────────────────────────────────────────

export const sendMessage = (to: string, content: string) =>
	invoke<ReceivedMessage>('send_message', { to, content });

export const getMessages = () => invoke<ReceivedMessage[]>('get_messages');

// ── Updates ───────────────────────────────────────────────────────────────────

export interface UpdateInfo { version: string; body?: string; }
export const checkUpdate   = () => invoke<UpdateInfo | null>('check_update');
export const installUpdate = () => invoke<void>('install_update');

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
