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
	TopicRank,
	TopicLookup,
	TopicInvitePreview,
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

/** M17 W3 — the zero-network share-code inspector: parses the code LOCALLY (no relay, no
 *  `resolve_peer`) and returns the embedded npub, its §7 fingerprint (single-sourced in hb-core), and
 *  whether the code carries a browse-key. The card render path calls only this + `validateShareCode`
 *  — both local — so a chat history full of codes costs ZERO relay round-trips. Resolution
 *  (`pasteKey`/`follow`) fires only on the user's click. */
export interface ShareCodeInfo {
	npub: string;
	fingerprint: { words: string[]; colorHex: string };
	has_browse_key: boolean;
}

export const shareCodeInfo = (code: string) =>
	invoke<ShareCodeInfo>('share_code_info', { code });

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

/** Full pre-flight of the backup at `path` before any destructive step: runs the real KDF,
 *  decrypt, and parse so a wrong passphrase / truncated / oversized archive rejects HERE, while
 *  the local identity is still on disk (QURATOR-126 — INV-8: no wipe before validation). */
export const validateBackup = (passphrase: string | null, path: string) =>
	invoke<void>('validate_backup', { passphrase, path });

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

/** Whether a collection's source root is currently reachable (timeout-bounded — a dead/slow SMB or
 *  removable mount can't hang). The Home list greys a collection until this returns true, and re-checks
 *  on a slow tick so a mount coming back online fills it back in. */
export const collectionSourceAccessible = (slug: string) =>
	invoke<boolean>('collection_source_accessible', { slug });

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

/** Unpublish a collection (spec §4): publishes a zeroed tombstone KIND_LISTING at the same `d`
 *  (QURATOR-138 owner ruling 2026-08-30 — conforming relays REPLACE the listing with it), plus the
 *  best-effort NIP-09 deletion (Public only — a Private collection's gift-wrapped events are
 *  ephemeral-keyed and cannot be deleted by this identity), drops the local published marker, and
 *  refreshes the profile teaser. The Home UI no longer calls this directly — Delete
 *  (`deleteCollection`) is the single retract-and-remove affordance; this stays for the backend
 *  contract and any future non-destructive retract. */
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
 *  file. `path` comes from the save dialog; Hoardbook writes the file and moves no collection files
 *  (INV-4′). Since M18 W4 this is the FALLBACK for when the transport can't connect — see
 *  [`sendFullList`]. */
export const exportManifest = (slug: string, path: string) =>
	invoke<void>('export_manifest', { slug, path });

/** M18 W4: **the fulfil verb.** Approve one asker's request for one collection — the backend builds
 *  the manifest, proves it fits the 8 MiB transport ceiling *before* promising anything, mints a
 *  ticket bound to that single approval, and DMs it. Fires only from an explicit click; Hoardbook
 *  never auto-sends (M17 ruling #4). Still moves no collection files (INV-4′) — a manifest is a
 *  listing, not a file. */
export const sendFullList = (npub: string, slug: string, askNonce?: string) =>
	invoke<void>('send_full_list', { npub, slug, askNonce: askNonce ?? null });

/** Carrier 4 (QURATOR-79) — the re-serve verb: hand `npub` a cached copy of `authorNpub`'s manifest,
 *  straight from the cache browsing put it in (nothing is built; the backend pins the served copy's
 *  author before promising anything). Fires only from an explicit click (M17 ruling #4). The wrapper
 *  exists so every Tauri call site lives in this file (QURATOR-171) — the chat page used to invoke
 *  the command through a raw dynamic import with this exact argument shape. */
export const sendCachedManifest = (npub: string, authorNpub: string, slug: string, askNonce?: string) =>
	invoke<void>('send_cached_manifest', {
		npub,
		authorNpub,
		slug,
		askNonce: askNonce ?? null,
	});

/** M18 W4: redeem a transport ticket that arrived by DM — dial, fetch, and consume through the
 *  unchanged M16 W4 gates (author pinned to the peer, slug bound to the ticket, signature verified
 *  before decrypt, completeness required).
 *
 *  **Called on arrival, never behind a "redeem later" button.** Redemption is immediate by owner
 *  ruling (2026-07-30), and the backend has no deferred entry point to bind one to. A failure does
 *  not spend the ticket, so retrying is safe. */
export const redeemManifestTicket = (
	npub: string,
	ticketJson: string,
	newestFingerprint?: string,
) =>
	invoke<ImportedManifest>('redeem_manifest_ticket', {
		npub,
		ticketJson,
		newestFingerprint: newestFingerprint ?? null,
	});

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
	/** v0.12.10 diagnostic: the loop's wakeup counter at the last state write — a rising count on
	 *  a frozen report proves the loop is alive but stuck in an await (vs. never spawned at all). */
	loopWakeups: number;
	/** v0.12.10 diagnostic: a breadcrumb written before each await in the cycle, so if an await
	 *  never returns the panel still shows where it wedged. */
	stage: string;
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
	/** M17 W5.2 — who, of the counted npubs, the poll just saw and when. Same single fetch the count
	 *  comes from (`online === fresh.length`), so the contact list gets per-contact presence with no
	 *  extra relay query. Empty while the count is unknown; absent on a pre-W5 cached payload. */
	fresh?: PresenceSeen[];
}

/** One "saw this npub's presence beacon at this time" pair from the online poll. */
export interface PresenceSeen {
	npub: string;
	/** RFC3339 — the beacon's `created_at`, i.e. THEIR presence, not our poll time. */
	seen_at: string;
}

/** Best-effort, cached online count (relay-derived). Returns immediately; refreshes in the background. */
export const onlineCount = () => invoke<OnlineCount>('online_count');

// ── Browse / Contacts ─────────────────────────────────────────────────────────

/** `code` is a pasted share code (bare npub or full `hbk…`). */
export const pasteKey = (code: string) => invoke<CachedPeer>('paste_key', { code });

/** `petname` is the M13 W5 seam — an optional user-supplied nickname set at follow-time, overriding
 *  the auto-derived one. Pass undefined/omit to keep the auto-derived petname.
 *
 *  M20 W2: `resolvedPeer` is the peer the lookup already resolved (`pasteKey`'s result). Passing it
 *  lets the Rust `follow` skip a SECOND `resolve_peer` after the user commits — the fix for the
 *  "add is slow / resolves twice" defect. Omit (e.g. chat Unlock, which has no pre-resolved peer) to
 *  fall back to resolving from `code` as before. */
export const follow = (
	code: string,
	groupName?: string | null,
	petname?: string,
	resolvedPeer?: CachedPeer,
) =>
	invoke<void>('follow', {
		code,
		groupName: groupName ?? null,
		petname: petname ?? null,
		resolvedPeer: resolvedPeer ?? null,
	});

export const getContacts = () => invoke<CachedPeer[]>('get_contacts');

export const unfollowContact = (npub: string) => invoke<void>('unfollow_contact', { npub });

export const refreshContact = (npub: string) => invoke<CachedPeer>('refresh_contact', { npub });

/** M16 W4 — the result of importing a `.hbmanifest`: the full-tree collection (fade lifted), and
 *  `stale` when the manifest predates the teaser the browser is showing (imported anyway, with a warn).
 *
 *  QURATOR-79 carrier 4 provenance: `served_by` names the peer whose cached copy arrived (None ⇒ the
 *  author served it directly). Additive-optional, absent on every pre-carrier-4 serve. The envelope's
 *  own clock is `collection.manifest_imported_at` — there is no second one here, and deliberately no
 *  cache-time field: a `cached_at` was declared here for months with no producer and was removed in
 *  QURATOR-172 #2. Only the REDEEM path produces `served_by`; `import_manifest` hardcodes None. */
export interface ImportedManifest {
	slug: string;
	collection: Collection;
	created_at: number;
	stale: boolean;
	served_by?: string;
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

/** The §6 Discovery result: ranked hit cards + whether the cap truncated the set (M20 W3). When
 *  `capped` is `true`, more candidates existed than the client cap kept, and the UI surfaces a
 *  "showing first N" affordance rather than silently presenting a capped slice as everyone. */
export interface PeerSearchResult {
	hits: PeerSearchHit[];
	capped: boolean;
}

/** Search public teasers by tag (AND) / content-type (OR). ≥1 filter is required (the backend
 *  rejects an empty search — no unfiltered global peer list). QURATOR-70: a SINGLE tag term matches
 *  name/bio/tags fuzzily; two+ tag terms are strict AND-on-tags (the chip affordance makes the
 *  second term a real observed tag, so narrowing never silently switches search kind). */
export const searchPeers = (tags: string[], contentTypes: string[]) =>
	invoke<PeerSearchResult>('search_peers', { tags, contentTypes });

/** QURATOR-70 — the set of tags this node has observed (contacts' teaser tags + own profile tags),
 *  for the Discover tag-chip autocomplete. Pure local-cache read — no relay round-trip. Lowercased,
 *  deduped, alphabetically sorted. */
export const discoverObservedTags = () =>
	invoke<string[]>('discover_observed_tags');

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

/** Carrier 4 (QURATOR-79) — the ask-origination half: DM `npub` (peer C) a structured request for
 *  `authorNpub`'s (peer A's) manifest, so C can re-serve it from its cache. Answer-only and
 *  human-mediated by design (design §5): this promises nothing about whether C holds it — C's
 *  client turns a recognised forward-request into a card only when it actually does. One relay
 *  write; the ask is recorded server-side under the (peer, author)-scoped ledger key. */
export const requestManifestFrom = (
	npub: string,
	authorNpub: string,
	slug: string,
	fingerprintSeen: string,
	teaserEventId?: string,
) =>
	invoke<void>('request_manifest_from', {
		npub,
		authorNpub,
		slug,
		fingerprintSeen,
		teaserEventId: teaserEventId ?? null,
	});

/** M17 W7.1a — the persisted ask-trace map (npub|slug → {fingerprint_seen, sent_at}), so the Browse
 *  paywall can read back the asked-state across restarts. Pure local read, no relay I/O. The ask is
 *  recorded INSIDE `request_manifest` after `send_dm_inner` resolves — a failed publish leaves no trace. */
export interface ManifestAsk {
	fingerprint_seen: string;
	sent_at: string;
	/** The nonce minted for this ask (owner ruling ①). A ticket auto-redeems only if it echoes this
	 *  exact value. Empty for a trace written before the ruling — and an empty nonce never matches,
	 *  so those asks simply stop auto-dialling until the user asks again. */
	nonce?: string;
}
export const getManifestAsks = () =>
	invoke<Record<string, ManifestAsk>>('get_manifest_asks');

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

// ── Portable self-updater (Windows loose exe) ──────────────────────────────────
// The portable build isn't an NSIS install, so the NSIS updater can't touch it. This path fetches the
// signed portable binary, verifies it under the SAME minisign key, and swaps the running exe in place.

export interface PortableUpdateInfo { version: string; notes?: string; }
/** True if this build is the portable (loose-exe) build — the UI routes to the portable updater when so,
 *  else to the NSIS updater above (the regression path). */
export const updaterIsPortable = () => invoke<boolean>('updater_is_portable');
/** Check for a newer portable release (reads the signed portable.json manifest). Null if up to date. */
export const checkPortableUpdate = () => invoke<PortableUpdateInfo | null>('check_portable_update');
/** Download + verify the newer portable binary, swap the running exe in place, and relaunch. */
export const applyPortableUpdate = () => invoke<void>('apply_portable_update');

// ── Groups ────────────────────────────────────────────────────────────────────

export const groupsGet    = () => invoke<Group[]>('groups_get');
/** `color` (M13 W5) is an optional CSS hex string for the group chip. */
export const groupsCreate = (name: string, color?: string) =>
	invoke<Group>('groups_create', { name, color: color ?? null });
/** Create a group pre-populated with members (M22 W1). `npubs` are de-duplicated; `[]` ≡ `groupsCreate`. */
export const groupsCreateWithMembers = (name: string, npubs: string[], color?: string) =>
	invoke<Group>('groups_create_with_members', { name, npubs, color: color ?? null });
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

// ── Private-collection audience (M21 W5) ──────────────────────────────────────
// Decoupled from contact groups by owner ruling (2026-08-04): joining a group or topic never
// enrols anyone here. Only the explicit per-contact toggle does.

/** List the npubs who receive every Private collection you publish. */
export const privateAudienceList = () => invoke<string[]>('private_audience_list');
/** Add (`receives = true`) or remove (`receives = false`) a single npub from the Private audience. */
export const privateAudienceSet = (npub: string, receives: boolean) =>
	invoke<void>('private_audience_set', { npub, receives });

// ── Diagnostics (QURATOR-65) ───────────────────────────────────────────────────
// The log/diagnostics surface that makes bug reports diagnosable. `copyDiagnostics` returns the
// header + capped log tail as a string; the UI owns the clipboard write. `revealLogFolder` opens
// the OS file manager at <app_data_dir>/logs.

/** The diagnostics text: header + capped tail of the current log file. Ready to paste. */
export const copyDiagnostics = () => invoke<string>('copy_diagnostics');

/** Open the OS file manager at the log directory. Creates it if missing (first launch). */
export const revealLogFolder = () => invoke<void>('reveal_log_folder');

/**
 * QURATOR-139 — open the GitHub repository in the system browser. Takes no arguments: the URL is
 * hard-coded in the Rust command (`commands::diagnostics::REPO_URL`), so the webview can never aim
 * the opener at anything else.
 */
export const openRepoPage = () => invoke<void>('open_repo_page');

// QURATOR-68 — the NAT classification token for the Settings → Diagnostics UI. One of
// "no-nat" | "nat" | "cgnat" | "unknown" | "undetermined" (before the first probe completes).
// The mapped address is never returned — only the classification (INV: peer/self addresses are
// the H4/MT2 harvest shape and never leave the machine via this surface).
export type NatClassification = 'no-nat' | 'nat' | 'cgnat' | 'unknown' | 'undetermined';
export const natClassification = () => invoke<NatClassification>('nat_classification');

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
	isPrivate: boolean,
) => invoke<TopicView>('topic_create', { name, description, private: isPrivate });

/** Edit a Topic's description after creation (devtest v0.12.1 #8). The name is immutable; a public
 *  Topic re-announces so discovery reflects the new blurb. */
export const topicUpdateMeta = (topicId: string, description: string) =>
	invoke<TopicView>('topic_update_meta', { topicId, description });

/** Discover all public Topics under a root category/primitive (devtest v0.12.1 #7) — pass a single
 *  root (e.g. `['video']`); the backend returns every public Topic beneath it, activity-ranked. */
export const topicDiscover = (tags: string[]) =>
	invoke<DiscoveredTopic[]>('topic_discover', { tags });

/** The W1 PAINT path (QURATOR-143): every public Topic under ALL the given roots in ONE relay read,
 *  with `member_count_estimate: null` everywhere — zero member_count round trips before first
 *  render. Ranking (ordering) is the lazy `topicRank` half, run after paint. */
export const topicDiscoverPaint = (tags: string[]) =>
	invoke<DiscoveredTopic[]>('topic_discover_paint', { tags });

/** The W1 LAZY-RANK half (QURATOR-143): fetch the spoofable member count for exactly the rows sent
 *  (bounded to concurrency 8 in hb-net), returning `(topic_id, count)` pairs count-desc. Send ONLY
 *  the rows that will actually be drawn — bounding the wave to the screen is this caller's half of
 *  the relay-citizenship contract. Each row carries its NAME alongside the id (QURATOR-148): the
 *  name derives the public-join credential a non-member's aliveness read recovers the topic key
 *  with — without it `alive_count` can only ever come back null (unknown). */
export const topicRank = (topics: { topic_id: string; name: string }[]) =>
	invoke<TopicRank[]>('topic_rank', { topics });

/** Join-first lookup (devtest #11): does this public Topic name already have a room? Never call for
 *  a private Topic (no announce to find). */
export const topicLookup = (name: string) => invoke<TopicLookup>('topic_lookup', { name });

export const topicJoinPublic = (name: string) =>
	invoke<TopicView>('topic_join_public', { name });

/** Redeem a private-Topic invite addressed to me, bound to the topic_id the user previewed/consented
 *  to (W8 substitution guard — a relay swapping in a different valid invite is rejected). Returns the
 *  joined Topic, or null if none found. */
export const topicRedeemInvite = (expectedTopicId: string) =>
	invoke<TopicView | null>('topic_redeem_invite', { expectedTopicId });

/** Preview a pending private-Topic invite WITHOUT committing (W8 consent gate): reveals the topic
 *  name/description + the invite issuer's npub so the UI can ask for explicit acknowledgment first.
 *  The follow-up `topicRedeemInvite` re-fetches and redeems the same invite. */
export const topicPreviewInvite = () => invoke<TopicInvitePreview | null>('topic_preview_invite');

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
