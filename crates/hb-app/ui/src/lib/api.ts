import { invoke as _invoke } from '@tauri-apps/api/core';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	if (!isTauri) return Promise.reject(new Error(`Tauri not available (cmd: ${cmd})`));
	return _invoke<T>(cmd, args);
}
import type {
	CachedPeer,
	Collection,
	DirectoryPeer,
	IdentityInfo,
	Profile,
	ReceivedChannelMessage,
	ReceivedMessage,
	ScanOptions,
	ShareSettings,
} from './types.js';

// ── Identity ─────────────────────────────────────────────────────────────────

export const generateKeypair = () => invoke<IdentityInfo>('generate_keypair');

export const getIdentity = () => invoke<IdentityInfo | null>('get_identity');

export const getHbId = () => invoke<string>('get_hb_id');

export const validateHbId = (hb_id: string) => invoke<boolean>('validate_hb_id', { hbId: hb_id });

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

/** Returns [available, takenByPubkey]. */
export const checkNameAvailable = (displayName: string) =>
	invoke<[boolean, string | null]>('check_name_available', { displayName });

// ── Collections ───────────────────────────────────────────────────────────────

export const scanDirectory = (opts: ScanOptions) =>
	invoke<Collection>('scan_directory', { opts });

export const getCollections = () => invoke<Collection[]>('get_collections');

export const deleteCollection = (slug: string) => invoke<void>('delete_collection', { slug });

export const publishCollection = (slug: string) =>
	invoke<void>('publish_collection', { slug });

export const updateCollectionMeta = (slug: string, description: string | undefined, contentType: string[], languages: string[], sorted: boolean) =>
	invoke<void>('update_collection_meta', { slug, description, contentType, languages, sorted });

// ── Settings ──────────────────────────────────────────────────────────────────

export interface Settings {
	relay_urls: string[];
	allow_dms: boolean;
	recommended: boolean;
}

export const getSettings = () => invoke<Settings>('get_settings');

export const saveSettings = (settings: Settings) => invoke<void>('save_settings', { settings });

export const checkRelay = (url: string) => invoke<void>('check_relay', { url });

// ── Browse / Contacts ─────────────────────────────────────────────────────────

export const pasteKey = (hb_id: string) => invoke<CachedPeer>('paste_key', { hbId: hb_id });

export const follow = (hb_id: string) => invoke<void>('follow', { hbId: hb_id });

export const getContacts = () => invoke<CachedPeer[]>('get_contacts');

export const unfollowContact = (hb_id: string) => invoke<void>('unfollow_contact', { hbId: hb_id });

export const refreshContact = (hb_id: string) => invoke<CachedPeer>('refresh_contact', { hbId: hb_id });

export const setContactTags = (hb_id: string, tags: string[]) =>
	invoke<void>('set_contact_tags', { hbId: hb_id, tags });

export const getDirectory = () => invoke<DirectoryPeer[]>('get_directory');

// ── Sharing ───────────────────────────────────────────────────────────────────

export const getShareSettings = (slug: string) =>
	invoke<ShareSettings>('get_share_settings', { slug });

export const saveShareSettings = (slug: string, settings: ShareSettings) =>
	invoke<void>('save_share_settings', { slug, settings });

export const requestDownload = (
	peer_hb_id: string,
	peer_node_addr: string | null,
	slug: string,
	path: string,
	save_path: string,
) => invoke<number>('request_download', { peerHbId: peer_hb_id, peerNodeAddr: peer_node_addr, slug, path, savePath: save_path });

// ── Chat ──────────────────────────────────────────────────────────────────────

export const sendMessage = (to: string, content: string) =>
	invoke<ReceivedMessage>('send_message', { to, content });

export const getMessages = () => invoke<ReceivedMessage[]>('get_messages');

export const getChannelMessages = (channel: string) =>
	invoke<ReceivedChannelMessage[]>('get_channel_messages', { channel });

export const postChannelMessage = (channel: string, content: string) =>
	invoke<ReceivedChannelMessage>('post_channel_message', { channel, content });

// ── Updates ───────────────────────────────────────────────────────────────────

export interface UpdateInfo { version: string; body?: string; }
export const checkUpdate   = () => invoke<UpdateInfo | null>('check_update');
export const installUpdate = () => invoke<void>('install_update');
