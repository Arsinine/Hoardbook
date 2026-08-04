<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	// M17 W7.1b: the manifest export path reuses the same save dialog as Home → ⋯ → Export (no new
	// export logic — this is a second entry point to the shipped `export_manifest` Tauri command).
	import { save as saveDialog } from '@tauri-apps/plugin-dialog';
	import { contacts, identity, inboxMessages, sentMessages, readWatermarks, toast, dmRequests, announceSeen, collections } from '$lib/stores.js';
	import {
		getMessages,
		sendMessage,
		pasteKey,
		follow,
		validateShareCode,
		shareCodeInfo,
		topicList,
		topicChannel,
		topicPost,
		getContacts,
		dmRequests as fetchDmRequests,
		dmRequestAccept,
		dmRequestDecline,
		dmBlock,
		groupsGet,
		groupsCreate,
		groupsSetTrusted,
		contactUpdateGroups,
		advanceReadWatermark,
		topicAnnounceMarkSeen,
		getShareCode,
		relayStatus,
		type RelayHealth,
		getCollections,
		exportManifest,
		sendFullList,
		redeemManifestTicket,
		getManifestAsks,
		getSettings,
	} from '$lib/api.js';
	import { relayWhyHint } from '$lib/relay-health.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import AddContactDialog from '$lib/components/AddContactDialog.svelte';
	import CreateGroupDialog from '$lib/components/CreateGroupDialog.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import ShareCodeCard from '$lib/components/ShareCodeCard.svelte';
	import ManifestFulfilCard from '$lib/components/ManifestFulfilCard.svelte';
	// M17 W4: HintMarker carries the pinned SHARE_MY_CODE_WARNING (free-text help, not a §8 anchor —
	// the FeatureTooltip registry is drift-guarded to exactly five keys, so a sixth is not the tool).
	import HintMarker from '$lib/components/HintMarker.svelte';
	import { DM_POLL_VISIBLE_MS, CHANNEL_REFRESH_EVERY_TICKS, ONLINE_POLL_VISIBLE_MS } from '$lib/poll-lifecycle.js';
	import { renderFingerprint } from '$lib/identity-display.js';
	import { contactDisplayName } from '$lib/contact-display.js';
	import { requestBadge, sortRequests, requestPreview, canReply, REQUEST_EXPLAINER, manifestRequestHint } from '$lib/request-inbox.js';
	// M17 W7.1b: the manifest-request fulfilment card. The capability (`export_manifest`) is fully
	// wired on Home; this is its second entry point — surfaced where the request lands. The card's
	// state is derived PURELY (zero network on render); the export Tauri call fires only on click.
	import { manifestFulfilFor, MANIFEST_EXPORTED_TOAST } from '$lib/manifest-fulfil.js';
	// M18 W4: the fulfil verb's two halves. The owner's "Send the full list" mints a ticket and DMs
	// it; the asker's side recognises that ticket and redeems it ON ARRIVAL — there is deliberately no
	// deferred redemption entry point in the backend to bind a button to (owner ruling 2026-07-30).
	import {
		parseTransportTicket,
		transportTicketHint,
		RedemptionLedger,
		SEND_FULL_LIST_TOAST,
		ticketAnswersOurAsk,
		askIdentity,
	} from '$lib/transport-ticket.js';
	import TransportTicketCard from '$lib/components/TransportTicketCard.svelte';
	import ContactPicker from '$lib/components/ContactPicker.svelte';
	import { filterConversations, filterTopics, composeRecipientKind, isComposeToSelf } from '$lib/chat-filter.js';
	import { peerPreview, peersWithHistory, relativeTime } from '$lib/chat-preview.js';
	// M17 W2: the ask-access intent populates the composer draft from one pure copy source, without
	// sending (no auto-send is structural — the helper only returns text, never publishes).
	import { applyAskAccessIntent } from '$lib/ask-access.js';
	// M17 W4: "Share my code" grant leg — the composer affordance fetches get_share_code (LOCAL) and
	// inserts the hbk1… string at the cursor via a pure splice helper. No confirm modal: insert-then-
	// send is already two deliberate acts. The helper NEVER sends — it returns the spliced draft and
	// the caret position to restore; the human presses Send.
	import { SHARE_MY_CODE_WARNING, insertAtCursor, withdrawInsert } from '$lib/share-my-code.js';
	// M17 W3: received-share-code detection (consume leg). The card is produced by a LOCAL parse
	// (regex candidate scan + validate_share_code + share_code_info, zero network) cached per message
	// id; resolution (paste_key/follow) fires only on click. Detection helper is pure; the Tauri calls
	// live here so the cache + zero-network-render invariant is pinned by the route test.
	import { extractShareCodeCandidate, shareCodeCandidates } from '$lib/share-code-detect.js';
	import type { ShareCodeInfo } from '$lib/api.js';
	import { sortChannelPostsAscending, resolveTopicParam, interleaveChannel } from '$lib/topics-view.js';
	import { latestFromPeer, unreadByPeer } from '$lib/unread-view.js';
	import type { CachedPeer, ReceivedMessage, TopicView, ChannelPost, AnnouncementView, DmRequestView, Group } from '$lib/types.js';

	let loading = $state(false);
	let sending = $state(false);
	let selectedPeer: CachedPeer | null = $state(null);
	let draft = $state('');
	// M17 W4 review: in-flight guard for the share-code fetch, and the binding of an inserted grant
	// to the conversation it was raised in (the draft itself is global — see selectPeer).
	let sharingCode = $state(false);
	// M17 W5.3: relay reachability, read from the same store the Contacts topbar uses. Drives ONE
	// muted line — the DM poll swallows its own errors, so this is the only place the user can learn
	// that a quiet inbox is actually an unreachable one.
	let relayHealth: RelayHealth[] = $state([]);
	let relayHint = $derived(relayWhyHint(relayHealth));
	let sharedCodeInDraft: { npub: string; code: string } | null = $state(null);
	let threadEl: HTMLElement | undefined = $state();
	// M17 W2: the ask-access intent focuses the composer (spec: an existing draft is not clobbered —
	// "just focus the composer"); refs let the intent paths do that after the pane renders.
	let draftEl: HTMLTextAreaElement | undefined = $state();
	let composeBodyEl: HTMLTextAreaElement | undefined = $state();
	let searchQuery = $state('');
	// devtest v0.12.1 #4: a `/chat?peer=<npub>` deep-link (from double-clicking a contact) opens that
	// conversation. Guarded so it fires once per distinct param, and re-evaluates as `$contacts` loads.
	let peerDeepLinked = $state('');

	// ── Topic channels (M11) — a Topic you've joined surfaces here as a persistent channel. The
	//    channel ENTRY lasts as long as your membership (durable, §11), but its posts are 24h-ephemeral
	//    (wiped server-side via NIP-40 + the local filter in `topic_channel`). Posting lives here now,
	//    not on the Topics page (which keeps join/leave/roster/invite).
	let topics: TopicView[] = $state([]);
	let selectedTopic: TopicView | null = $state(null);
	let channelPosts: ChannelPost[] = $state([]);
	let channelAnnouncements: AnnouncementView[] = $state([]); // M13 Part A: read-only member broadcasts
	// devtest #6: announcements are interleaved with posts by timestamp (not pinned above).
	let channelItems = $derived(interleaveChannel(channelPosts, channelAnnouncements));
	let channelDraft = $state('');
	let channelSending = $state(false);

	// ── Q7 Request inbox (M13 Part B) — a stranger's DM is quarantined here, never merged into the
	//    conversation list. `viewingRequests` selects the Requests section in the right pane;
	//    `selectedRequest` (once set) drills into one sender's bucket.
	let viewingRequests = $state(false);
	let selectedRequest: DmRequestView | null = $state(null);

	// ── M17 W3: received-share-code card (consume leg) ──────────────────────────────────────────
	// Per-message-id cache of the detected share code (`shareCodeInfo`, parsed LOCALLY — zero network
	// at render). A message id → { code, info }; `null` means "scanned, no valid code". Once detected,
	// the card renders purely from cached local data; re-renders of the history are free. Resolution
	// (paste_key/follow) fires only on the user's click — detection never touches the relay.
	let detectedCodes: Record<string, { code: string; info: ShareCodeInfo } | null> = $state({});
	// In-flight detection guard (idempotency): mirrors the `fetchingNames` pattern. The cache write
	// happens only AFTER the async validate/shareCodeInfo calls resolve; a re-render/poll in that
	// window would otherwise start a duplicate parse. Local calls only — no network — but the guard
	// keeps the cache + detectedCodes writes single-flight per message id.
	const detectingIds = new Set<string>();
	// Session flag: a code the user has clicked Unlock on (flips the card to "Unlocked ✓"). Keyed by
	// code so re-render after the contacts refresh still shows the unlocked state.
	let unlockedCodes: Set<string> = $state(new Set());
	// In-flight unlock (idempotency guard: a double-click Unlock is a no-op while one is resolving).
	let unlockingCode: string | null = $state(null);
	// Third-party forwarded code → AddContactDialog (petname + group; a NEW relationship, full ritual).
	let addContactDialogOpen = $state(false);
	let addContactTarget: { code: string; info: ShareCodeInfo } | null = $state(null);

	// ── M17 W7.1b: manifest-request fulfilment card ─────────────────────────────────────────────
	// The owner's `big_relay_url` (Settings), read once on mount. When set, the fulfilment card's
	// secondary line points the owner at the big relay ("they'll get the rest automatically"); when
	// empty, a muted one-liner links to that Settings field instead. Honest about the last mile.
	let bigRelayUrl = $state('');
	// In-flight export guard (idempotency: a double-click Export is a no-op while one is resolving).
	let exportingSlug: string | null = $state(null);


	// ── Compose-to-npub (spec §9 first-contact deep link from Discovery) ─────────────────────────
	let composeOpen = $state(false);
	let composeTo = $state('');
	let composeBody = $state('');
	let composeSending = $state(false);
	// M21 W3: the compose modal's "+" affordance opens a ContactPicker. Selecting a contact sets
	// `composeTo` to their npub, which then flows through the SAME validation + send path as typing.
	let composePickerOpen = $state(false);

	async function loadTopics() {
		try { topics = await topicList(); } catch { /* relay unreachable */ }
	}

	async function loadRequests() {
		try { dmRequests.set(await fetchDmRequests()); } catch { /* relay/store unreachable */ }
	}

	async function loadChannel(topicId: string) {
		try {
			const view = await topicChannel(topicId);
			channelPosts = sortChannelPostsAscending(view.posts);
			channelAnnouncements = view.announcements;
			// devtest #2: reading the channel clears its Topics nav badge — advance the seen watermark to
			// the newest announcement and mirror it into the store so the badge updates without a refetch.
			const newest = view.announcements.reduce((m, a) => Math.max(m, a.ts), 0);
			if (newest > 0) {
				announceSeen.update((s) => (newest > (s[topicId] ?? 0) ? { ...s, [topicId]: newest } : s));
				topicAnnounceMarkSeen(topicId, newest).catch(() => { /* non-fatal — reseeds next launch */ });
			}
		} catch { /* relay unreachable */ }
	}

	async function selectTopic(t: TopicView) {
		selectedTopic = t;
		selectedPeer = null;
		viewingRequests = false;
		selectedRequest = null;
		channelPosts = [];
		channelAnnouncements = [];
		await loadChannel(t.topic_id);
		await tick();
		scrollToBottom();
	}

	function openRequests() {
		viewingRequests = true;
		selectedRequest = null;
		selectedPeer = null;
		selectedTopic = null;
	}

	function openRequest(r: DmRequestView) {
		selectedRequest = r;
	}

	// Petname-dialog wiring (M13 W5 Slice 2): accepting a Request now asks for an optional petname +
	// group first, via the same shared AddContactDialog used on Contacts, instead of always passing
	// `null` straight through to `dmRequestAccept`.
	let acceptDialogOpen = $state(false);
	let acceptTarget: DmRequestView | null = $state(null);
	let createGroupOpen = $state(false);
	let groups: Group[] = $state([]);

	async function loadGroups() {
		try { groups = await groupsGet(); } catch { /* non-fatal */ }
	}

	function openAcceptDialog(r: DmRequestView) {
		acceptTarget = r;
		acceptDialogOpen = true;
	}

	async function handleCreateGroup(detail: { name: string; color: string; trusted: boolean }) {
		const { name, color, trusted } = detail;
		try {
			await groupsCreate(name, color);
			if (trusted) await groupsSetTrusted(name, true);
			await loadGroups();
		} catch (e) { toast(String(e), 'error'); }
	}

	async function completeAccept(r: DmRequestView, petname: string | null, group: string | null) {
		try {
			const drained = await dmRequestAccept(r.npub, petname);
			inboxMessages.update((prev) => {
				const seenKeys = new Set(prev.map((m) => `${m.from}|${m.sent_at}`));
				const fresh = drained.filter((m) => !seenKeys.has(`${m.from}|${m.sent_at}`));
				return [...prev, ...fresh];
			});
			dmRequests.update((prev) => prev.filter((x) => x.npub !== r.npub));
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			if (group) {
				try {
					await contactUpdateGroups(r.npub, [group]);
					contacts.set(await getContacts());
				} catch { /* non-fatal */ }
			}
			viewingRequests = false;
			selectedRequest = null;
			const peer = $contacts.find((c) => c.npub === r.npub);
			if (peer) await selectPeer(peer);
			toast('Contact added', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleAcceptSave(detail: { petname: string; group: string | null }) {
		if (!acceptTarget) return;
		const r = acceptTarget;
		acceptDialogOpen = false;
		acceptTarget = null;
		await completeAccept(r, detail.petname, detail.group);
	}

	async function handleAcceptSkip() {
		if (!acceptTarget) return;
		const r = acceptTarget;
		acceptDialogOpen = false;
		acceptTarget = null;
		await completeAccept(r, null, null);
	}

	async function handleDecline(r: DmRequestView) {
		try {
			await dmRequestDecline(r.npub);
			dmRequests.update((prev) => prev.filter((x) => x.npub !== r.npub));
			selectedRequest = null;
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleBlock(r: DmRequestView) {
		try {
			await dmBlock(r.npub);
			dmRequests.update((prev) => prev.filter((x) => x.npub !== r.npub));
			selectedRequest = null;
			toast('Blocked', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// ── M17 W3: received-share-code card (consume leg) ──────────────────────────────────────────
	/** Stable per-message key for the detection cache. `ReceivedMessage` carries no unique id and
	 *  `sent_at` is second-granular, so two same-second DMs from one sender would collide on
	 *  from|sent_at alone — include the content to disambiguate distinct bubbles (identical content in
	 *  the same second is genuinely the same card, so a collision there is harmless). */
	const messageKey = (m: { from: string; sent_at: string; content: string }) =>
		`${m.from}|${m.sent_at}|${m.content}`;
	/** Detect a share code in a message ONCE per message id and cache the local-parse result. The
	 *  card render path invokes only LOCAL Tauri commands (`validate_share_code` + `share_code_info`)
	 *  — zero network. `paste_key`/`follow` never fire here; they fire in `handleUnlock` on click.
	 *  Returns the cached info, or `null` when the message has no valid code (plain text). */
	async function ensureDetected(messageId: string, text: string): Promise<{ code: string; info: ShareCodeInfo } | null> {
		if (messageId in detectedCodes) return detectedCodes[messageId];
		if (detectingIds.has(messageId)) return null; // an in-flight parse is already running
		detectingIds.add(messageId);
		try {
			// Validate EXACTLY the candidate strings `extractShareCodeCandidate` will test (raw tokens
			// AND the over-long-token prefix slices), via the shared single-source helper — otherwise a
			// two-codes-no-separator paste has verdicts only for the raw over-long token (absent/false)
			// and the slice-recovery path never renders a card.
			const verdicts = new Map<string, boolean>();
			for (const c of shareCodeCandidates(text)) {
				if (verdicts.has(c)) continue;
				try { verdicts.set(c, await validateShareCode(c)); }
				catch { verdicts.set(c, false); }
			}
			const code = extractShareCodeCandidate(text, (c) => verdicts.get(c) ?? false);
			if (!code) {
				detectedCodes = { ...detectedCodes, [messageId]: null };
				return null;
			}
			try {
				const info = await shareCodeInfo(code);
				const entry = { code, info };
				detectedCodes = { ...detectedCodes, [messageId]: entry };
				return entry;
			} catch {
				detectedCodes = { ...detectedCodes, [messageId]: null };
				return null;
			}
		} finally {
			detectingIds.delete(messageId);
		}
	}

	/** A derived view of detected codes for a message — safe for the template (returns null until the
	 *  async detection settles, then reactivity picks up the cached value). */
	function detectedFor(messageId: string): { code: string; info: ShareCodeInfo } | null {
		return detectedCodes[messageId] ?? null;
	}

	/** Click on "Unlock browsing" — the ONE network surface on the card, behind an explicit click.
	 *  `pasteKey` resolves the peer via relay, then `follow` re-adds with the full code (preserving
	 *  the existing contact's petname/groups/local tags — the M15 bug is fixed at the Rust seam).
	 *  Idempotent: a double-click while one is resolving is a no-op (`unlockingCode` guard). */
	async function handleUnlock(code: string) {
		if (unlockingCode === code || unlockedCodes.has(code)) return;
		unlockingCode = code;
		try {
			// pasteKey networks (relay resolve); follow re-adds with the full code, preserving local
			// state. The existing petname is kept because the user already named this contact.
			await pasteKey(code);
			await follow(code);
			contacts.set(await getContacts());
			unlockedCodes = new Set([...unlockedCodes, code]);
			toast('Browsing unlocked', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			unlockingCode = null;
		}
	}

	/** Click on "Add contact" — a forwarded third-party code. Opens the standard AddContactDialog
	 *  (petname + group — a NEW relationship, full ritual applies). Resolution happens on save.
	 *  Takes the clicked card's detected entry DIRECTLY (the {code, info} the card rendered from) —
	 *  a previous version re-looked it up from `detectedCodes` by npub and could bind the wrong code
	 *  when two cards share an npub (a bare npub vs a full hbk card). */
	function handleAddContact(entry: { code: string; info: ShareCodeInfo }) {
		addContactTarget = entry;
		addContactDialogOpen = true;
	}

	async function handleAddContactSave(detail: { petname: string; group: string | null }) {
		if (!addContactTarget) return;
		const target = addContactTarget;
		addContactDialogOpen = false;
		addContactTarget = null;
		try {
			await follow(target.code, detail.group ?? undefined, detail.petname || undefined);
			contacts.set(await getContacts());
			unlockedCodes = new Set([...unlockedCodes, target.code]);
			toast('Contact added', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	function handleAddContactSkip() {
		addContactDialogOpen = false;
		addContactTarget = null;
	}

	// M17 W7.1b — the fulfil click: the exact `handleExport(slug,'manifest')` path from Home, with the
	// honest post-export copy (Hoardbook writes the file and moves no bytes — send it yourself).
	// No new export logic; this is the second entry point to the shipped `export_manifest` command.
	// Idempotent: a double-click while one save dialog is resolving is a no-op (`exportingSlug` guard).
	async function handleExportManifest(slug: string) {
		if (exportingSlug === slug) return;
		exportingSlug = slug;
		try {
			const path = await saveDialog({
				defaultPath: `${slug}.hbmanifest`,
				filters: [{ name: 'Hoardbook manifest', extensions: ['hbmanifest'] }],
			});
			if (!path) return;
			await exportManifest(slug, path);
			const filename = path.split(/[\\/]/).pop() ?? `${slug}.hbmanifest`;
			toast(MANIFEST_EXPORTED_TOAST(filename), 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			exportingSlug = null;
		}
	}

	// M18 W4 — the fulfil verb. The whole of the owner's decision is this click: `send_full_list`
	// builds the manifest, refuses it up front if it exceeds the transport ceiling (naming export in
	// the error), mints a ticket bound to this one approval, records it, and DMs it. Nothing auto-fires
	// (M17 ruling #4), and export stays on the card beside it for when the transport can't connect.
	// Guarded per slug so a double-click cannot mint two approvals for one request.
	let sendingFullList: string | null = $state(null);
	async function handleSendFullList(slug: string, askNonce?: string) {
		if (sendingFullList === slug) return;
		const npub = selectedPeer?.npub;
		if (!npub) return;
		sendingFullList = slug;
		try {
			await sendFullList(npub, slug, askNonce);
			toast(SEND_FULL_LIST_TOAST(slug), 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			sendingFullList = null;
		}
	}

	// M18 W4 — the asker's half. A ticket DM is redeemed the first time it is SEEN, not on a click:
	// redemption is immediate by owner ruling, and the backend exposes no "redeem later" path. The
	// ledger is keyed by `request_id` so the same DM re-rendered on every 3s poll (and re-read from
	// the encrypted cache across restarts) fires exactly once — without it, a successfully-received
	// manifest would show its owner a stream of "already redeemed" errors.
	//
	// The redeemed tree is NOT plumbed across tabs: the redemption caches the verified envelope
	// through the same path a file import uses, so Browse's existing cache resolution upgrades the
	// truncated teaser on its own. One hand-off, not two.
	const redemptions = new RedemptionLedger();
	let redemptionTick = $state(0); // bumped to re-render the cards; the ledger itself is not reactive
	async function redeem(npub: string, requestId: string, ask: string, ticketJson: string) {
		try {
			await redeemManifestTicket(npub, ticketJson);
			redemptions.succeed(requestId, ask);
			// The backend consumed the ask, so our copy is stale — re-read it. Without this the
			// in-memory trace would still authorize a second ticket for the rest of the session,
			// which is exactly the standing authorization the nonce removes.
			manifestAsks = null;
			asksAttempt += 1;
		} catch (e) {
			redemptions.fail(requestId, ask, String(e));
		} finally {
			redemptionTick += 1;
		}
	}

	// M18 W4 — the local ask trace (`npub|slug` → when we asked). The gate below fails CLOSED while it
	// is null, so a slow load can never open an auto-dial window.
	//
	// **The retry is not optional.** A single swallowed rejection used to leave `manifestAsks` null
	// forever: the effect had no other reactive dependency to re-run it, so every legitimate ticket
	// rendered as "unsolicited" with no action, and automatic delivery could not recover for the rest
	// of the session. Fail-closed must stay *recoverable*, or a transient local read error becomes a
	// permanent feature outage that also blames the sender.
	let manifestAsks = $state<Record<string, { nonce?: string }> | null>(null);
	let asksLoading = false;
	let asksAttempt = $state(0);
	$effect(() => {
		void asksAttempt; // re-runs when a retry is scheduled
		if (manifestAsks !== null || asksLoading) return;
		asksLoading = true;
		void getManifestAsks()
			.then((a) => {
				manifestAsks = a;
			})
			.catch(() => {
				// Bounded backoff, capped — a persistently failing read must not spin.
				const delay = Math.min(30_000, 2_000 * 2 ** Math.min(asksAttempt, 4));
				setTimeout(() => {
					asksAttempt += 1;
				}, delay);
			})
			.finally(() => {
				asksLoading = false;
			});
	});

	/** Fire the first redemption for a ticket we have just rendered. Returns the current state so the
	 *  card can render it in the same pass. Safe to call on every render — the ledger claims once.
	 *
	 *  **The gate is an IP-exposure control.** Redeeming DIALS the peer, so it hands them our address,
	 *  and this fires on render. The ticket must echo the nonce we minted for *this* ask (owner ruling
	 *  ①): matching on peer+slug alone was a standing authorization — satisfied once, then exploitable
	 *  forever with any node address the peer chose. We dial only in reply to a specific thing we sent. */
	function redemptionFor(
		npub: string | null,
		slug: string,
		ticketNonce: string | undefined,
		ticketJson: string,
		requestId: string,
	) {
		void redemptionTick; // read so this re-evaluates when a redemption settles
		// Trace not loaded yet (or its read failed): we cannot tell solicited from unsolicited, so we
		// dial nothing AND say so honestly rather than accusing the sender. Recoverable — the loader
		// above retries, and this re-evaluates when it lands.
		if (manifestAsks === null) return { kind: 'unverified' } as const;
		if (!npub || !ticketNonce || !ticketAnswersOurAsk(manifestAsks, npub, slug, ticketNonce)) {
			return { kind: 'unsolicited' } as const;
		}
		// Scope the claim to the ASK, not the ticket: one nonce must not authorize N concurrent dials
		// to N peer-chosen addresses.
		const ask = askIdentity(npub, slug, ticketNonce);
		if (redemptions.claim(requestId, ask)) {
			void redeem(npub, requestId, ask, ticketJson);
		}
		return redemptions.get(requestId) ?? ({ kind: 'unsolicited' } as const);
	}

	/** Retry after a failure. **The gate is re-checked here even though an unsolicited ticket can
	 *  never reach the `failed` state** (it never touches the ledger, so `claimRetry` refuses it).
	 *  Relying on that is a transitive argument through the ledger's state machine; the invariant
	 *  "we dial only in reply to something we sent" is worth stating at *every* site that can dial, so
	 *  a future change to the ledger cannot quietly open one. Same discipline as the slug binding,
	 *  which is checked on both sides for two different reasons. */
	function retryRedemption(
		npub: string | null,
		slug: string,
		ticketNonce: string | undefined,
		ticketJson: string,
		requestId: string,
	) {
		if (!npub || !ticketNonce || !ticketAnswersOurAsk(manifestAsks, npub, slug, ticketNonce)) return;
		const ask = askIdentity(npub, slug, ticketNonce);
		if (!redemptions.claimRetry(requestId, ask)) return;
		redemptionTick += 1;
		void redeem(npub, requestId, ask, ticketJson);
	}

	async function sendChannelPost() {
		if (!selectedTopic || !channelDraft.trim() || channelSending) return;
		channelSending = true;
		const body = channelDraft.trim();
		channelDraft = '';
		try {
			await topicPost(selectedTopic.topic_id, body);
			await loadChannel(selectedTopic.topic_id);
			await tick();
			scrollToBottom();
		} catch (e) {
			toast(String(e), 'error');
			channelDraft = body;
		} finally {
			channelSending = false;
		}
	}

	function channelKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendChannelPost(); }
	}

	// M13 Part B (Q7): the conversation list is contacts ONLY — a stranger's DM no longer merges in
	// here at all (the request pane, above, replaces the old inboxOnlyPeers merge). M15 W6 replaced
	// the inline latestMessageTime with chat-preview.ts's peerPreview (time + text in one pass).


	// Cache of display_name for npubs not in $contacts (Request-bucket senders). Populated lazily by
	// fetchNonContactNames(); never causes re-triggers because we only write when a key is absent.
	let peerNameCache: Record<string, string> = {};
	const fetchingNames = new Set<string>(); // prevents duplicate in-flight fetches

	async function fetchNonContactNames(npubs: string[]) {
		for (const npub of npubs) {
			if (fetchingNames.has(npub) || peerNameCache[npub]) continue;
			fetchingNames.add(npub);
			try {
				const fetched = await pasteKey(npub);
				if (fetched.profile?.display_name) {
					peerNameCache = { ...peerNameCache, [npub]: fetched.profile.display_name };
				}
			} catch { /* relay unreachable or peer has no profile — fall back to shortId */ }
		}
	}


	// Resolve display name for a sender hb_id — contacts first, then fetched cache.
	function senderName(hb_id: string): string {
		if (hb_id === myId) return 'You';
		const contact = $contacts.find(c => c.npub === hb_id);
		// Petname-first via the shared helper (M13 W5); cache/shortId fallbacks unchanged.
		if (contact && (contact.petname?.trim() || contact.profile?.display_name)) return contactDisplayName(contact);
		if (peerNameCache[hb_id]) return peerNameCache[hb_id];
		return shortId(hb_id);
	}

	onMount(() => {
		refreshInbox();
		loadGroups();

		// M17 W7.1b: the fulfilment card derives its state from the owner's own drafts (the request
		// matches one of your Public collections → "Export manifest…"). Refresh once on mount so the
		// card is honest without the owner having to visit Home. The settings read picks up the
		// `big_relay_url` for the secondary hint (best-effort — never blocks on a relay/FS hiccup).
		getCollections().then((cs) => collections.set(cs)).catch(() => { /* non-fatal */ });
		getSettings().then((s) => { bigRelayUrl = s.big_relay_url ?? ''; }).catch(() => { /* non-fatal */ });


		// Discovery first-contact deep link (spec §9): `/chat?compose=<npub-or-sharecode>` prefills
		// and opens the compose modal. M17 W2: `&intent=ask-access` populates the modal body from
		// `askAccessDraft` WITHOUT sending (a human always presses Send). An existing draft is never
		// clobbered — the helper returns the untouched text when the user already typed something.
		const composeParam = $page.url.searchParams.get('compose');
		if (composeParam) {
			composeTo = composeParam;
			composeOpen = true;
			const intent = $page.url.searchParams.get('intent');
			if (intent) {
				const petname = $page.url.searchParams.get('petname') ?? '';
				const applied = applyAskAccessIntent(intent, composeBody, petname);
				composeBody = applied.draft;
				if (applied.focus) tick().then(() => composeBodyEl?.focus());
			}
		}

		// Topic channel deep link (devtest #15): `/chat?topic=<topic_id>` from the Topics page's
		// "Open this Topic's channel in Chat" link selects the channel directly, no second click.
		const topicParam = $page.url.searchParams.get('topic');
		loadTopics().then(() => {
			if (!topicParam) return;
			const t = resolveTopicParam(topicParam, topics);
			if (t) selectTopic(t);
			else toast("That Topic channel isn't available — you may have left it.", 'error');
		});

		// Local DM poll while the chat page is open. M12 W1 Decision B: backed off from 4 s (the
		// dominant connect source against the relays) and visibility-gated — paused while the window
		// is hidden so it doesn't churn relay connections in the background; resumes on show.
		let pollTick = 0;
		const fastPoll = setInterval(async () => {
			if (!$identity || document.hidden) return;
			// devtest v0.12.4 #1: DMs poll every 3s (cheap incremental fetch), but the open Topic
			// channel's low-velocity 24h posts refresh on a slower cadence so we don't over-poll the relay.
			pollTick++;
			if (selectedTopic && pollTick % CHANNEL_REFRESH_EVERY_TICKS === 0) loadChannel(selectedTopic.topic_id);
			try {
				const msgs = await getMessages();
				// Detect genuinely new messages for the selected peer and auto-scroll.
				if (selectedPeer) {
					const prevCount = $inboxMessages.filter(m => m.from === selectedPeer!.npub).length;
					const nextCount = msgs.filter(m => m.from === selectedPeer!.npub).length;
					if (nextCount > prevCount) {
						inboxMessages.set(msgs);
						// The open conversation just got new messages — re-advance its watermark too,
						// so an open thread never accumulates a phantom unread count (devtest #16).
						const latest = latestFromPeer(msgs, selectedPeer.npub);
						if (latest) {
							readWatermarks.update((w) => ({ ...w, [selectedPeer!.npub]: latest }));
							advanceReadWatermark(selectedPeer.npub, latest).catch(() => { });
						}
						await tick();
						scrollToBottom();
						return;
					}
				}
				inboxMessages.set(msgs);
			} catch { /* relay unreachable */ }
			// Q7: refresh the Request inbox right after the main inbox poll.
			loadRequests();
		}, DM_POLL_VISIBLE_MS);

		// W5.3: relay health on the slow tick (not the 3s DM cadence — reachability changes slowly
		// and this must not add churn against the relays).
		const readRelayHealth = async () => {
			try { relayHealth = await relayStatus(); } catch { /* keep last health */ }
		};
		readRelayHealth();
		const healthPoll = setInterval(() => { if (!document.hidden) readRelayHealth(); }, ONLINE_POLL_VISIBLE_MS);

		return () => {
			clearInterval(fastPoll);
			clearInterval(healthPoll);
		};
	});

	async function refreshInbox() {
		if (!$identity) return;
		loading = true;
		try {
			const msgs = await getMessages();
			inboxMessages.set(msgs);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			loading = false;
		}
		loadRequests();
	}

	async function selectPeer(peer: CachedPeer) {
		// W4 review: a share code inserted for someone else does NOT follow the switch (the draft is
		// global; Send targets whoever is selected now). Withdraw the grant, keep the typed text.
		if (sharedCodeInDraft && sharedCodeInDraft.npub !== peer.npub) {
			draft = withdrawInsert(draft, sharedCodeInDraft.code);
			sharedCodeInDraft = null;
		}
		selectedPeer = peer;
		selectedTopic = null;
		viewingRequests = false;
		selectedRequest = null;
		// Opening a conversation reads it: advance the peer's watermark to the newest message we
		// have from them (devtest #16 — the badge clears per-conversation, not on merely landing on
		// /chat). Optimistic local update + best-effort persist.
		const latest = latestFromPeer($inboxMessages, peer.npub);
		if (latest) {
			readWatermarks.update((w) => ({ ...w, [peer.npub]: latest }));
			advanceReadWatermark(peer.npub, latest).catch(() => { });
		}
		await tick();
		scrollToBottom();
	}

	async function handleSend() {
		if (!selectedPeer || !draft.trim() || sending) return;
		sending = true;
		const content = draft.trim();
		draft = '';
		try {
			const sent = await sendMessage(selectedPeer.npub, content);
			sharedCodeInDraft = null; // the grant left the composer
			sentMessages.update((prev) => [...prev, sent]);
			await tick();
			scrollToBottom();
		} catch (e) {
			toast(String(e), 'error');
			draft = content;
		} finally {
			sending = false;
		}
	}

	// M17 W4: "Share my code" grant leg. Fetches the LOCAL get_share_code (no network) and splices the
	// hbk1… string into the draft at the cursor via the pure helper. Does NOT send — insert-then-send
	// is already two deliberate acts, so there is no confirm modal (owner may override; we ship the
	// no-modal default). The sent bubble then renders via W3's card logic as the own-code inert card.
	async function handleShareMyCode() {
		if (sending || sharingCode || !selectedPeer) return;
		// Bind the grant to the conversation it was raised in: the draft is ONE global $state, so
		// without this the code follows a peer switch and Send hands our browse capability to
		// whoever is selected THEN. `sharingCode` is the in-flight guard (a double-click otherwise
		// splices two codes, and the second splice runs against a stale caret).
		const forPeer = selectedPeer.npub;
		sharingCode = true;
		try {
			const code = await getShareCode();
			// Switched away, or a Send started mid-fetch (which rewrites the draft under us) — drop
			// the insert rather than repopulating a composer the user just emptied.
			if (selectedPeer?.npub !== forPeer || sending) return;
			const start = draftEl?.selectionStart ?? draft.length;
			const end = draftEl?.selectionEnd ?? draft.length;
			const { value, cursor } = insertAtCursor(draft, code, start, end);
			draft = value;
			sharedCodeInDraft = { npub: forPeer, code };
			await tick();
			draftEl?.focus();
			draftEl?.setSelectionRange(cursor, cursor);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			sharingCode = false;
		}
	}

	// Compose-to-npub modal (spec §9): send() rebuilds a CachedPeer stub if the recipient wasn't
	// already a contact, so the composer can select straight into the new conversation.
	async function handleComposeSend() {
		const to = composeTo.trim();
		const content = composeBody.trim();
		if (!to || !content || composeSending) return;
		composeSending = true;
		try {
			const sent = await sendMessage(to, content);
			sentMessages.update((prev) => [...prev, sent]);
			composeOpen = false;
			composeTo = '';
			composeBody = '';
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			const peer = $contacts.find((c) => c.npub === sent.to) ?? ({
				npub: sent.to, browse_key_hex: undefined, petname: undefined, profile: undefined,
				collections: [], online: false, last_fetched: '', local_tags: [],
			} satisfies CachedPeer);
			await selectPeer(peer);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			composeSending = false;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			handleSend();
		}
	}

	function scrollToBottom() {
		if (threadEl) threadEl.scrollTop = threadEl.scrollHeight;
	}

	function shortId(hb_id: string) {
		return hb_id.length > 16 ? hb_id.slice(0, 8) + '…' + hb_id.slice(-4) : hb_id;
	}

	function formatTime(iso: string) {
		return new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	function formatDate(iso: string) {
		return new Date(iso).toLocaleDateString([], { month: 'short', day: 'numeric' });
	}


	function viewProfile(peer: CachedPeer) {
		goto('/contacts');
	}

	let sortedRequests = $derived(sortRequests($dmRequests));
	let requestCount = $derived(requestBadge($dmRequests));
	let myId = $derived($identity?.npub ?? '');
	// M15 W6: only contacts with actual DM history appear in the list (messageless contacts are
	// reachable via compose + the Contacts tab; the pane still renders from selectedPeer, and a new
	// thread's row appears after the first send). Sorted newest-first by the latest message's ISO time.
	let historyPeers = $derived(peersWithHistory($inboxMessages, $sentMessages));
	let allConversationPeers = $derived([...$contacts]
		.filter((c) => historyPeers.has(c.npub))
		.sort((a, b) => {
			// Sort by parsed ms (not string compare) so a non-UTC-offset timestamp can't misorder the
			// list (chorus catch — removes the "backend always emits UTC-Z" assumption).
			const aT = new Date(peerPreview($inboxMessages, $sentMessages, a.npub)?.time ?? 0).getTime();
			const bT = new Date(peerPreview($inboxMessages, $sentMessages, b.npub)?.time ?? 0).getTime();
			return bT - aT; // newest first
		}));
	// Wires the search box (devtest copy audit — it was dead): filters the visible rows only, never
	// the underlying stores.
	let visiblePeers = $derived(filterConversations(allConversationPeers, searchQuery, senderName));
	let visibleTopics = $derived(filterTopics(topics, searchQuery));
	let conversation = $derived(selectedPeer
		? [
				...$inboxMessages.filter((m) => m.from === selectedPeer!.npub),
				...$sentMessages.filter((m) => m.to === selectedPeer!.npub)
			].sort((a, b) => a.sent_at.localeCompare(b.sent_at))
		: []);
	// Eagerly fetch names for Request-bucket senders whenever the list changes.
	$effect(() => {
		fetchNonContactNames($dmRequests.map((r) => r.npub));
	});
	// devtest v0.12.1 #4: resolve the `?peer=<npub>` deep-link through selectPeer once the contact is
	// loaded. Re-runs when $contacts settles; a contact double-clicked in Contacts is always present,
	// so it opens their conversation pane (empty-thread until the first message, then it joins the list).
	// M17 W2: `&intent=ask-access` populates the composer draft from askAccessDraft WITHOUT sending.
	// No-clobber: if the user already typed a draft, the helper returns the untouched text and we just
	// focus the composer. The petname (carried via `&petname=`) personalises the greeting.
	$effect(() => {
		const npub = $page.url.searchParams.get('peer') ?? '';
		if (!npub || npub === peerDeepLinked) return;
		const peer = $contacts.find((c) => c.npub === npub);
		if (peer) {
			peerDeepLinked = npub;
			selectPeer(peer);
			const intent = $page.url.searchParams.get('intent');
			if (intent) {
				const petname = $page.url.searchParams.get('petname') ?? peer.petname ?? '';
				const applied = applyAskAccessIntent(intent, draft, petname);
				draft = applied.draft;
				if (applied.focus) tick().then(() => draftEl?.focus());
			}
		}
	});
	// devtest #16: derived straight from the persisted per-peer watermark, replacing the old
	// seenCounts in-memory snapshot (which reset to "no unread" on every remount).
	let unreadCounts = $derived(unreadByPeer($inboxMessages, $readWatermarks, myId));
	// Show a privacy notice if the selected peer is not in contacts (may have DMs restricted).
	let selectedIsContact = $derived(selectedPeer ? $contacts.some(c => c.npub === selectedPeer!.npub) : false);

	// M17 W3: detect share codes in the currently-visible messages (conversation thread OR the open
	// request bucket). Fires only LOCAL Tauri commands (validate + share_code_info) — zero network.
	// A history full of codes costs zero relay round-trips; the result is cached per message id so
	// re-renders are free. The message id key includes the content (see `messageKey`) so two
	// same-second DMs from one sender get distinct cache entries.
	$effect(() => {
		const scan = (messages: { from: string; sent_at: string; content: string }[]) => {
			for (const m of messages) {
				const id = messageKey(m);
				if (id in detectedCodes) continue;
				ensureDetected(id, m.content);
			}
		};
		if (selectedPeer) scan(conversation);
		if (selectedRequest) scan(selectedRequest.messages);
	});
</script>

{#if !$identity}
	<div class="no-identity">
		<p>No identity yet.</p>
		<a href="/settings" class="btn-primary">Go to Settings →</a>
	</div>
{:else}
	<div class="chat-frame">
		<!-- Conversation list -->
		<div class="convo-sidebar">
			<div class="convo-header">
				<span class="convo-title">Conversations</span>
				<div class="header-icons">
					<button class="icon-btn" onclick={() => (composeOpen = true)} title="New message">
						{@html icons.plus}
					</button>
					<button class="icon-btn" onclick={refreshInbox} disabled={loading} title="Refresh inbox">
						{@html icons.refresh}
					</button>
				</div>
			</div>
			<!-- W5.3: the DM poll's `catch {}` used to eat every relay failure, so a silently stale
			     inbox looked exactly like a quiet one. One muted line, same relay-health store the
			     Contacts topbar reads. No toast — this is chrome, not an event. -->
			{#if relayHint}
				<div class="relay-hint" title="Messages may be stale until a relay is reachable.">{relayHint} — inbox may be stale</div>
			{/if}
			<div class="convo-search">
				<div class="search-wrap">
					<span class="search-icon-sm">{@html icons.search}</span>
					<input class="search-bare" type="text" placeholder="Search…" bind:value={searchQuery} />
				</div>
			</div>
			<div class="convo-list">
				{#if visibleTopics.length > 0}
					<div class="convo-section-label">Channels</div>
					{#each visibleTopics as t (t.topic_id)}
						<button class="convo-item" class:convo-active={selectedTopic?.topic_id === t.topic_id} onclick={() => selectTopic(t)}>
							<div class="channel-hash">#</div>
							<div class="convo-info">
								<div class="convo-row">
									<span class="convo-name" class:convo-name-active={selectedTopic?.topic_id === t.topic_id}>{t.name}</span>
									{#if t.private}<span class="convo-lock" title="Private topic">🔒</span>{/if}
								</div>
							</div>
						</button>
					{/each}
				{/if}
				{#if $dmRequests.length > 0}
					<div class="convo-section-label">Requests</div>
					<button class="convo-item" class:convo-active={viewingRequests} onclick={openRequests}>
						<div class="channel-hash">🔔</div>
						<div class="convo-info">
							<div class="convo-row">
								<span class="convo-name" class:convo-name-active={viewingRequests}>Message requests</span>
								<span class="unread-badge">{requestCount}</span>
							</div>
						</div>
					</button>
				{/if}
				<div class="convo-section-label">Direct messages</div>
				{#if visiblePeers.length === 0}
					<div class="convo-empty">
						{allConversationPeers.length === 0 ? 'No conversations yet — add someone in Contacts to start one.' : 'No matches.'}
					</div>
				{:else}
					{#each visiblePeers as peer}
						{@const name = senderName(peer.npub)}
						{@const initial = name[0]?.toUpperCase() ?? '?'}
						{@const hue = avatarHue(initial)}
						{@const unread = unreadCounts[peer.npub] ?? 0}
						{@const active = selectedPeer?.npub === peer.npub}
						{@const preview = peerPreview($inboxMessages, $sentMessages, peer.npub)}
						<button class="convo-item" class:convo-active={active} onclick={() => selectPeer(peer)}>
							<Avatar letter={initial} size={34} {hue} picture={peer.profile?.picture} />
							<div class="convo-info">
								<div class="convo-row">
									<span class="convo-name" class:convo-name-active={active}>{name}</span>
									{#if preview}<span class="convo-time">{relativeTime(preview.time, new Date())}</span>{/if}
								</div>
								<div class="convo-preview-row">
									{#if preview}<span class="convo-preview-text">{preview.text}</span>{/if}
									{#if unread > 0}
										<span class="unread-badge">{unread}</span>
									{/if}
								</div>
							</div>
						</button>
					{/each}
				{/if}
			</div>
		</div>

		<!-- Conversation pane -->
		<div class="convo-pane">
			{#if selectedTopic}
				<!-- Topic channel: a persistent entry (your durable membership, §11) whose posts are
				     24h-ephemeral (server NIP-40 + the local filter in topic_channel). -->
				<div class="pane-header">
					<div class="channel-hash channel-hash-lg">#</div>
					<div class="pane-peer-info">
						<div class="pane-peer-row">
							<span class="pane-peer-name">{selectedTopic.name}</span>
							{#if selectedTopic.private}<span class="pill pill-offline">private</span>{/if}
						</div>
						<span class="channel-sub">Topic channel · each post disappears 24h after it's posted · manage in Topics</span>
					</div>
				</div>

				<div class="thread" bind:this={threadEl}>
					{#if channelItems.length === 0}
						<p class="thread-empty">No posts in the last 24h — posts here expire 24h after they're sent. Say something!</p>
					{:else}
						{#each channelItems as item (item.kind + '|' + (item.kind === 'post' ? item.post.author_npub + '|' + item.post.ts : item.announce.author_npub + '|' + item.announce.ts))}
							{#if item.kind === 'announce'}
								<div class="announce-banner">
									<span class="announce-icon">📣</span>
									<div class="announce-body">
										<span class="announce-author">{senderName(item.announce.author_npub)}</span>
										<p class="announce-text">{item.announce.body}</p>
										<span class="announce-time">{formatTime(new Date(item.announce.ts * 1000).toISOString())}</span>
									</div>
								</div>
							{:else}
								{@const isMe = item.post.author_npub === myId}
								<div class="bubble-wrap" class:bubble-me={isMe}>
									<div class="bubble" class:bubble-sent={isMe} class:bubble-recv={!isMe}>
										{#if !isMe}<span class="bubble-author">{senderName(item.post.author_npub)}</span>{/if}
										<p class="bubble-text">{item.post.body}</p>
										<span class="bubble-time">{formatTime(new Date(item.post.ts * 1000).toISOString())}</span>
									</div>
								</div>
							{/if}
						{/each}
					{/if}
				</div>

				<div class="composer">
					<div class="compose-box">
						<textarea
							class="compose-input"
							placeholder="Message #{selectedTopic.name}…"
							bind:value={channelDraft}
							onkeydown={channelKeydown}
							disabled={channelSending}
							rows="2"
						></textarea>
						<div class="compose-footer">
							<button class="btn-primary btn-send" onclick={sendChannelPost} disabled={!channelDraft.trim() || channelSending}>
								{channelSending ? '…' : 'Post'} <span>{@html icons.send}</span>
							</button>
						</div>
					</div>
				</div>
			{:else if viewingRequests}
				{#if !selectedRequest}
					<!-- Requests list: sorted newest-activity-first (Q7 — never merged into the main list). -->
					<div class="pane-header">
						<div class="channel-hash channel-hash-lg">🔔</div>
						<div class="pane-peer-info">
							<div class="pane-peer-row"><span class="pane-peer-name">Message requests</span></div>
							<span class="channel-sub">Quarantined until you accept, decline, or block</span>
						</div>
					</div>
					<div class="requests-explainer">{REQUEST_EXPLAINER}</div>
					<div class="thread">
						{#if sortedRequests.length === 0}
							<p class="thread-empty">No message requests.</p>
						{:else}
							{#each sortedRequests as r (r.npub)}
								{@const name = senderName(r.npub)}
								{@const initial = name[0]?.toUpperCase() ?? '?'}
								<button class="request-row" onclick={() => openRequest(r)}>
									<Avatar letter={initial} size={34} hue={avatarHue(initial)} />
									<div class="convo-info">
										<div class="convo-row">
											<span class="convo-name">{name}</span>
											<span class="unread-badge">{r.message_count}</span>
										</div>
										<div class="request-preview">{requestPreview(r)}</div>
										{#if r.fingerprint}
											<div class="request-fp" title="Identity fingerprint — check it before accepting a stranger">
												<span class="request-fp-swatch" style="background:{r.fingerprint.colorHex}"></span>
												{renderFingerprint(r.fingerprint)}
											</div>
										{/if}
									</div>
								</button>
							{/each}
						{/if}
					</div>
				{:else}
					{@const req = selectedRequest}
					{@const reqName = senderName(req.npub)}
					{@const isRequestContact = $contacts.some((c) => c.npub === req.npub)}
					<!-- Opened request: read-only messages + Accept/Decline/Block (no reply until accepted). -->
					<div class="pane-header">
						<Avatar letter={reqName[0]?.toUpperCase() ?? '?'} size={36} hue={avatarHue(reqName[0] ?? '?')} />
						<div class="pane-peer-info">
							<div class="pane-peer-row"><span class="pane-peer-name">{reqName}</span></div>
							<span class="mono">{shortId(req.npub)}</span>
						</div>
						<button class="btn-ghost btn-sm" onclick={() => (selectedRequest = null)}>← Back</button>
					</div>
					<div class="requests-explainer">{REQUEST_EXPLAINER}</div>
					<div class="thread">
						{#each req.messages as msg}
							<div class="bubble-wrap">
								<div class="bubble bubble-recv">
									<p class="bubble-text">{manifestRequestHint(msg.content) ?? transportTicketHint(msg.content) ?? msg.content}</p>
									<span class="bubble-time">{formatTime(msg.sent_at)}</span>
										{#if detectedFor(messageKey(msg))}
											{@const card = detectedFor(messageKey(msg))!}
											<!-- M17 W3 quarantine: the card renders for recognition, but ZERO action
											     buttons - Accept comes first, always (hard constraint #3). The card
											     itself shows the fingerprint so the user can recognise a known peer's
											     code before deciding to accept. -->
											<ShareCodeCard
												info={card.info}
												chatPeerNpub={null}
												ownNpub={myId}
												contacts={$contacts}
												quarantined={true}
												unlocked={false}
												onunlock={() => {}}
												unlocking={false}
												onaddcontact={() => {}}
											/>
										{/if}
										{#if manifestFulfilFor(msg.content, $collections, { quarantined: true })}
											{@const mf = manifestFulfilFor(msg.content, $collections, { quarantined: true })!}
											<!-- M17 W7.1b quarantine: the fulfilment card renders for recognition, but ZERO action
														buttons (Accept first, always — same rule as W3's ShareCodeCard). The state is derived
														PURELY so the owner can see what the request is about before deciding to accept. -->
											<ManifestFulfilCard
												state={mf.state}
												fingerprintSeen={mf.request.fingerprintSeen}
												hasBigRelay={bigRelayUrl !== ''}
												onexport={() => {}}
												onsend={() => {}}
												sending={false}
											/>
										{/if}
										{#if parseTransportTicket(msg.content)}
											{@const tk = parseTransportTicket(msg.content)!}
											<!-- M18 W4, quarantine: recognition only, and — the part that matters — NO
											     redemption fires. `redemptionFor` is not called here, so a stranger's
											     ticket cannot make this node dial them before its sender has been
											     accepted. Accept first, always (same rule as W3 and W7.1b). -->
											<TransportTicketCard
												slug={tk.slug}
												state={undefined}
												quarantined={true}
												onretry={() => {}}
											/>
										{/if}
								</div>
							</div>
						{/each}
					</div>
					{#if !canReply(isRequestContact)}
						<div class="composer request-actions">
							<button class="btn-primary" onclick={() => openAcceptDialog(req)}>Accept</button>
							<button class="btn-ghost" onclick={() => handleDecline(req)}>Decline</button>
							<button class="btn-ghost btn-danger" onclick={() => handleBlock(req)}>Block</button>
						</div>
					{/if}
				{/if}
			{:else if !selectedPeer}
				<div class="convo-empty-state">
					<p>Select a contact to view the conversation.</p>
					<p class="privacy-note">
						{@html icons.shield} Messages are end-to-end encrypted — relays never see who sent them or what they say.
					</p>
				</div>
			{:else}
				<!-- Header -->
				<div class="pane-header">
					<Avatar
						letter={(selectedPeer.profile?.display_name || selectedPeer.npub)[0].toUpperCase()}
						size={36}
						hue={avatarHue((selectedPeer.profile?.display_name || selectedPeer.npub)[0])}
						picture={selectedPeer.profile?.picture}
					/>
					<div class="pane-peer-info">
						<div class="pane-peer-row">
							<span class="pane-peer-name">{selectedPeer.profile?.display_name || shortId(selectedPeer.npub)}</span>
							{#if selectedPeer.online}
								<span class="pill pill-online"><span class="pill-dot"></span> Online</span>
							{:else}
								<span class="pill pill-offline">Offline</span>
							{/if}
						</div>
						<span class="mono">{shortId(selectedPeer.npub)}</span>
					</div>
					<!-- M15 W6: the always-on E2E banner is consolidated into this header shield (hover) +
					     the empty-thread note; the offline/not-a-contact banners stay (contextual). -->
					<span class="e2e-shield" title="End-to-end encrypted — relays see only that someone messaged this person, never the content or the sender.">{@html icons.shield}</span>
					<button class="btn-ghost btn-sm" onclick={() => { if (selectedPeer) viewProfile(selectedPeer); }}>View profile</button>
				</div>

				<!-- Offline notice -->
				{#if !selectedPeer.online}
					<div class="offline-banner">
						<span class="offline-dot"></span>
						<span>{selectedPeer.profile?.display_name || shortId(selectedPeer.npub)} is offline — they'll see your message the next time they open Hoardbook.</span>
					</div>
				{/if}

				<!-- Notice for message requests (sender not in recipient's contacts) -->
				{#if !selectedIsContact}
					<div class="request-banner">
						<span>This person may not have added you back — their privacy settings may filter your messages.</span>
					</div>
				{/if}

				<!-- Thread -->
				<div class="thread" bind:this={threadEl}>
					{#if conversation.length === 0}
						<p class="thread-empty">No messages yet. Say hello!</p>
					{:else}
						{#each conversation as msg, i}
							{@const isMe = msg.from === myId}
							{@const prevMsg = i > 0 ? conversation[i - 1] : null}
							{@const showDate = !prevMsg || formatDate(msg.sent_at) !== formatDate(prevMsg.sent_at)}
							{#if showDate}
								<div class="day-marker">
									<div class="day-line"></div>
									<span class="day-label">{formatDate(msg.sent_at)}</span>
									<div class="day-line"></div>
								</div>
							{/if}
							<div class="bubble-wrap" class:bubble-me={isMe}>
								<div class="bubble" class:bubble-sent={isMe} class:bubble-recv={!isMe}>
									<p class="bubble-text">{manifestRequestHint(msg.content) ?? transportTicketHint(msg.content) ?? msg.content}</p>
									<span class="bubble-time">{formatTime(msg.sent_at)}</span>
									{#if detectedFor(messageKey(msg))}
										{@const card = detectedFor(messageKey(msg))!}
										<!-- M17 W3: the card is an ADDENDUM below the verbatim message text (never a
										     replacement). Zero-network at render: the info was parsed locally (cached
										     per message id); resolution (paste_key/add-contact) fires only on a click.
										     A sent-bubble own-code flips to the inert "Your share code" state inside
										     the card. -->
										<ShareCodeCard
											info={card.info}
											chatPeerNpub={selectedPeer?.npub ?? null}
											ownNpub={myId}
											contacts={$contacts}
											quarantined={false}
											unlocked={unlockedCodes.has(card.code)}
											onunlock={() => handleUnlock(card.code)}
											unlocking={unlockingCode === card.code}
											onaddcontact={() => handleAddContact(card)}
										/>
									{/if}
									{#if manifestFulfilFor(msg.content, $collections, { quarantined: false })}
										{@const mf = manifestFulfilFor(msg.content, $collections, { quarantined: false })!}
										<!-- M17 W7.1b: the fulfilment card is an ADDENDUM below the verbatim message
													text (same rule as W3). Zero-network at render: state is derived PURELY from
													(request, own drafts); the export save dialog fires only on click. -->
										<ManifestFulfilCard
											state={mf.state}
											fingerprintSeen={mf.request.fingerprintSeen}
											hasBigRelay={bigRelayUrl !== ''}
											onexport={(slug) => handleExportManifest(slug)}
											onsend={(slug) => handleSendFullList(slug, mf.request.askNonce)}
											sending={sendingFullList === mf.state.slug}
										/>
									{/if}
									{#if parseTransportTicket(msg.content)}
										{@const tk = parseTransportTicket(msg.content)!}
										<!-- M18 W4: the ticket card. Unlike the two cards above it, this one fires its
										     action on RENDER rather than on a click — the asker already asked, the owner
										     has already decided, and the backend has no deferred redemption path. The
										     ledger keyed by request_id is what makes "on render" fire exactly once. -->
										<TransportTicketCard
											slug={tk.slug}
											state={redemptionFor(
												selectedPeer?.npub ?? null,
												tk.slug,
												tk.askNonce,
												msg.content,
												tk.requestId,
											)}
											quarantined={false}
											onretry={() =>
												retryRedemption(
													selectedPeer?.npub ?? null,
													tk.slug,
													tk.askNonce,
													msg.content,
													tk.requestId,
												)}
										/>
									{/if}
								</div>
							</div>
						{/each}
					{/if}
				</div>

				<!-- Compose -->
				<div class="composer">
					<div class="compose-box">
						<textarea
							class="compose-input"
							placeholder="Type a message…"
							bind:value={draft}
							bind:this={draftEl}
							onkeydown={handleKeydown}
							disabled={sending}
							rows="2"
						></textarea>
						<div class="compose-footer">
							<!-- M17 W4: "Share my code" grant leg — inserts the LOCAL get_share_code at the
							     cursor; never sends (insert-then-send is two deliberate acts, so no confirm modal). -->
							<div class="compose-footer-left">
								<button
									type="button"
									class="icon-btn share-my-code-btn"
									onclick={handleShareMyCode}
									disabled={sending || sharingCode}
									aria-label="Share my code"
									title="Share my code"
								>
									{@html icons.key}
								</button>
								<HintMarker text={SHARE_MY_CODE_WARNING} label="Share my code" />
							</div>
							<button
								class="btn-primary btn-send"
								onclick={handleSend}
								disabled={!draft.trim() || sending}
							>
								{sending ? '…' : 'Send'} <span>{@html icons.send}</span>
							</button>
						</div>
					</div>
				</div>
			{/if}
		</div>
	</div>
{/if}

<!-- Petname + group dialog shown before accepting a Request (M13 W5 Slice 2). -->
<AddContactDialog
	open={acceptDialogOpen}
	displayName={acceptTarget ? senderName(acceptTarget.npub) : ''}
	{groups}
	onsave={handleAcceptSave}
	onskip={handleAcceptSkip}
	onnewGroup={() => (createGroupOpen = true)}
	oncancel={() => { acceptDialogOpen = false; acceptTarget = null; }}
/>
<!-- M17 W3: third-party forwarded share code → the standard add funnel (petname + group — a NEW
     relationship, full ritual applies). Surfaces when a received code's embedded npub ≠ chat peer. -->
<AddContactDialog
	open={addContactDialogOpen}
	displayName={addContactTarget?.info.npub ? shortId(addContactTarget.info.npub) : ''}
	{groups}
	onsave={handleAddContactSave}
	onskip={handleAddContactSkip}
	onnewGroup={() => (createGroupOpen = true)}
	oncancel={handleAddContactSkip}
/>

<CreateGroupDialog open={createGroupOpen} oncreate={handleCreateGroup} oncancel={() => (createGroupOpen = false)} />

<!-- Compose-to-npub (spec §9 first-contact deep link) — a + icon-btn beside refresh opens this. -->
<Modal open={composeOpen} title="New message" onclose={() => (composeOpen = false)}>
	<div class="compose-fields">
		<!-- M21 W3: the free-text recipient stays (owner: "in addition to its current form"); a
		     "Contacts" button next to it opens the ContactPicker, which sets composeTo to a chosen
		     contact's npub. That value then flows through the SAME validation + send path as typing. -->
		<div class="recipient-row">
			<input class="hb-input" placeholder="npub or hbk share code…" bind:value={composeTo} />
			<button type="button" class="link" onclick={() => (composePickerOpen = true)}>Contacts</button>
		</div>
		{#if composeTo.trim() && isComposeToSelf(composeTo, $identity?.npub ?? '', $identity?.share_code ?? '')}
			<div class="compose-hint">That's your own ID.</div>
		{:else if composeTo.trim() && composeRecipientKind(composeTo) === 'invalid'}
			<div class="compose-hint">Doesn't look like an npub or share code — sending will reject it if it's wrong.</div>
		{/if}
		<textarea class="hb-input hb-textarea compose-modal-input" placeholder="Message…" bind:value={composeBody} bind:this={composeBodyEl} rows="3"></textarea>
	</div>
	{#snippet actions()}
		<button class="btn-ghost" onclick={() => (composeOpen = false)}>Cancel</button>
		<button class="btn-primary" disabled={!composeTo.trim() || !composeBody.trim() || composeSending || isComposeToSelf(composeTo, $identity?.npub ?? '', $identity?.share_code ?? '')} onclick={handleComposeSend}>
			{composeSending ? '…' : 'Send'}
		</button>
	{/snippet}
</Modal>

<!-- M21 W3 — compose "+": pick a contact to prefill composeTo (single-select; chat sends to one
     recipient). Stacked above the compose modal. The chosen npub reuses the same validation path. -->
<ContactPicker
	open={composePickerOpen}
	title="Message a contact"
	confirmLabel="Select"
	contacts={$contacts}
	myNpub={$identity?.npub ?? ''}
	onselect={(npub) => { composeTo = npub; composePickerOpen = false; }}
	onclose={() => (composePickerOpen = false)}
/>

<style>
	.no-identity {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		gap: 12px;
		color: var(--fg-muted);
	}

	.chat-frame { display: flex; height: 100%; overflow: hidden; }

	/* Conversation list sidebar */
	.convo-sidebar {
		width: 240px;
		flex-shrink: 0;
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		background: var(--bg);
	}

	.convo-header {
		padding: 16px 16px 10px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.convo-title { font-size: 14px; font-weight: 600; }

	.header-icons { display: flex; gap: 4px; align-items: center; }

	.icon-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-muted);
		display: flex;
		padding: 2px;
	}
	.icon-btn:disabled { opacity: 0.5; }

	.convo-search { padding: 10px 12px; border-bottom: 1px solid var(--divider); }

	/* W5.3 — muted chrome, not an alert: it states a fact about the relays, and gets out of the way. */
	.relay-hint {
		padding: 6px 12px;
		font-size: 11px;
		color: var(--fg-dim);
		border-bottom: 1px solid var(--divider);
	}

	.search-wrap {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 10px;
		height: 30px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}

	.search-icon-sm { color: var(--fg-dim); display: flex; }

	.search-bare {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 12.5px;
		color: var(--fg);
	}
	.search-bare::placeholder { color: var(--fg-dim); }

	.convo-list { flex: 1; overflow-y: auto; padding: 6px 8px; }

	.convo-empty { padding: 12px; font-size: 12px; color: var(--fg-dim); }

	/* M15 W7: removed the dead .convo-divider rule (unreferenced). */

	.convo-item {
		width: 100%;
		display: flex;
		gap: 10px;
		align-items: center;
		padding: 10px;
		background: transparent;
		border: none;
		border-radius: 7px;
		cursor: pointer;
		color: inherit;
		font-family: inherit;
		margin-bottom: 2px;
		text-align: left;
	}
	.convo-item:hover { background: var(--bg-elev1); }
	.convo-active { background: var(--bg-elev2); }

	.convo-info { flex: 1; min-width: 0; }

	.convo-row { display: flex; justify-content: space-between; align-items: center; gap: 4px; }

	.convo-name { font-size: 13px; font-weight: 500; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
	.convo-name-active { font-weight: 600; }

	/* devtest v0.12.1 #5: a private-topic marker reads as a lock, not a filled dot (the old amber dot
	   looked like a permanent unread/notification bubble). */
	.convo-lock { font-size: 10px; line-height: 1; flex-shrink: 0; opacity: 0.6; }

	/* Topic channels (M11) */
	.convo-section-label {
		padding: 10px 12px 4px;
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 1px;
		color: var(--fg-dim);
	}
	.channel-hash {
		width: 34px; height: 34px; flex-shrink: 0;
		display: flex; align-items: center; justify-content: center;
		border-radius: 8px;
		background: var(--bg-elev2);
		color: var(--fg-muted);
		font-size: 17px; font-weight: 700;
	}
	.channel-hash-lg { width: 36px; height: 36px; font-size: 18px; }
	.channel-sub { font-family: var(--font-mono); font-size: 11px; color: var(--fg-dim); }
	.bubble-author { display: block; font-size: 10.5px; font-weight: 600; color: var(--accent); margin-bottom: 2px; }

	.convo-preview-row { display: flex; align-items: center; margin-top: 2px; gap: 4px; }
	/* M15 W6: last-message preview + relative time in the conversation list. */
	.convo-time { font-size: 10.5px; color: var(--fg-dim); flex-shrink: 0; font-feature-settings: 'tnum'; }
	.convo-preview-text { font-size: 11.5px; color: var(--fg-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }

	.unread-badge {
		font-size: 10px;
		padding: 1px 6px;
		border-radius: 999px;
		background: var(--accent);
		color: var(--accent-text);
		font-weight: 700;
		min-width: 16px;
		text-align: center;
		font-feature-settings: 'tnum';
	}

	/* Conversation pane */
	.convo-pane {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: var(--bg-elev1);
	}

	.convo-empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 12px;
		padding: 32px;
		color: var(--fg-muted);
	}

	.privacy-note { font-size: 12px; color: var(--fg-dim); text-align: center; max-width: 320px; }

	.pane-header {
		padding: 12px 18px;
		border-bottom: 1px solid var(--border);
		display: flex;
		gap: 12px;
		align-items: center;
		background: var(--bg);
	}

	.pane-peer-info { flex: 1; min-width: 0; }

	.pane-peer-row { display: flex; align-items: center; gap: 8px; margin-bottom: 2px; }

	.pane-peer-name { font-weight: 600; font-size: 14px; }

	.mono { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	/* M15 W6: always-on privacy banner replaced by this subtle header shield (hover) + empty-thread note. */
	.e2e-shield { color: var(--accent); display: flex; margin-left: 4px; cursor: help; flex-shrink: 0; }
	.e2e-shield :global(svg) { width: 15px; height: 15px; }

	.offline-banner {
		padding: 7px 18px;
		background: color-mix(in oklch, var(--fg-dim) 8%, transparent);
		border-bottom: 1px solid var(--border);
		font-size: 11.5px;
		color: var(--fg-muted);
		display: flex;
		gap: 8px;
		align-items: center;
	}
	.offline-dot {
		width: 7px; height: 7px; border-radius: 50%;
		background: var(--fg-dim); flex-shrink: 0;
	}

	.request-banner {
		padding: 6px 18px;
		background: oklch(0.22 0.06 60 / 0.6);
		border-bottom: 1px solid oklch(0.45 0.12 60 / 0.3);
		font-size: 11.5px;
		color: oklch(0.82 0.12 60);
	}

	.thread {
		flex: 1;
		padding: 20px 24px;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.thread-empty { color: var(--fg-dim); font-size: 13px; text-align: center; padding-top: 32px; }

	.day-marker { display: flex; align-items: center; gap: 10px; margin: 12px 0 8px; }

	.day-line { flex: 1; height: 1px; background: var(--divider); }

	.day-label { font-size: 10.5px; color: var(--fg-dim); text-transform: uppercase; letter-spacing: 1px; white-space: nowrap; }

	.bubble-wrap { display: flex; margin-bottom: 4px; }
	.bubble-me { justify-content: flex-end; }

	.bubble {
		max-width: 70%;
		padding: 8px 12px;
		border-radius: 14px;
	}

	.bubble-sent {
		background: var(--accent);
		color: var(--accent-text);
		border-radius: 14px 14px 4px 14px;
	}

	.bubble-recv {
		background: var(--bg-elev2);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 14px 14px 14px 4px;
	}

	.bubble-text { font-size: 13px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; margin: 0; }

	.bubble-time { font-size: 10px; color: inherit; opacity: 0.6; display: block; text-align: right; margin-top: 3px; }

	.composer {
		padding: 14px;
		border-top: 1px solid var(--border);
		background: var(--bg);
	}

	.compose-box {
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 9px;
		padding: 10px 12px;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.compose-input {
		width: 100%;
		background: transparent;
		border: none;
		outline: none;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		resize: none;
		min-height: 36px;
	}
	.compose-input::placeholder { color: var(--fg-dim); }

	.compose-footer { display: flex; justify-content: flex-end; align-items: center; }

	/* M17 W4: the "Share my code" affordance sits left of Send (Send stays the rightmost primary).
	   margin-right:auto pushes it to the left edge under the parent's flex-end. */
	.compose-footer-left { display: flex; align-items: center; gap: 2px; margin-right: auto; }
	.share-my-code-btn { color: var(--fg-muted); }
	.share-my-code-btn:hover { color: var(--fg); }

	.btn-send {
		display: inline-flex; align-items: center; justify-content: center; gap: 5px;
		padding: 6px 14px;
		font-family: var(--font-ui); font-size: 12px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
		min-width: 68px;
	}
	.btn-send:disabled { opacity: 0.5; cursor: not-allowed; }

	/* Pills */
	.pill {
		display: inline-flex; align-items: center; gap: 5px;
		font-size: 10.5px; font-weight: 500;
		padding: 2px 8px; border-radius: 999px;
	}
	.pill-dot { width: 5px; height: 5px; border-radius: 50%; }
	.pill-online {
		color: var(--online);
		background: color-mix(in oklch, var(--online) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--online) 20%, transparent);
	}
	.pill-online .pill-dot { background: var(--online); }
	.pill-offline {
		color: var(--fg-muted);
		background: color-mix(in oklch, var(--fg-muted) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--fg-muted) 20%, transparent);
	}

	/* M15 W1: buttons unified on the app.css .btn system (local copies removed; .btn-send stays,
	   it's a chat-specific composer button not in the shared vocabulary). */

	/* Topic announcements (M13 Part A) — a highlighted, read-only broadcast above the ordinary posts. */
	.announce-banner {
		display: flex;
		gap: 10px;
		padding: 10px 14px;
		margin-bottom: 10px;
		background: var(--accent-soft);
		border: 1px solid var(--border);
		border-radius: 9px;
	}
	.announce-icon { font-size: 15px; line-height: 1; }
	.announce-body { flex: 1; min-width: 0; }
	.announce-author { display: block; font-size: 10.5px; font-weight: 600; color: var(--accent); margin-bottom: 2px; }
	.announce-text { font-size: 13px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; margin: 0; }
	.announce-time { font-size: 10px; color: var(--fg-dim); display: block; margin-top: 3px; }

	/* Q7 Request inbox */
	.requests-explainer {
		padding: 8px 18px;
		background: var(--accent-soft);
		border-bottom: 1px solid var(--border);
		font-size: 11.5px;
		color: var(--fg-muted);
	}
	.request-row {
		width: 100%;
		display: flex;
		gap: 10px;
		align-items: flex-start;
		padding: 10px;
		background: transparent;
		border: none;
		border-bottom: 1px solid var(--divider);
		border-radius: 7px;
		cursor: pointer;
		color: inherit;
		font-family: inherit;
		text-align: left;
	}
	.request-row:hover { background: var(--bg-elev1); }
	.request-preview { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }
	.request-fp {
		display: flex; align-items: center; gap: 5px;
		font-family: var(--font-mono); font-size: 10.5px; color: var(--fg-dim);
		margin-top: 4px;
	}
	.request-fp-swatch { width: 9px; height: 9px; border-radius: 50%; flex-shrink: 0; }
	.request-actions { display: flex; gap: 8px; justify-content: flex-end; }

	/* Compose-to-npub modal */
	.compose-hint { font-size: 11px; color: var(--fg-dim); }
	/* M20 W4: the compose modal's textarea + npub input compose the global .hb-input contract
	   (app.css); only the full-width + box-sizing layout stays here. */
	.compose-modal-input { width: 100%; box-sizing: border-box; }
	/* M15 W2: compose modal now uses Modal.svelte; only the field layout is local. */
	.compose-fields { display: flex; flex-direction: column; gap: 8px; }
	/* M21 W3: the recipient input + the "Contacts" affordance sit on one row. The input itself uses
	   the global .hb-input contract — only the row layout is local. */
	.recipient-row { display: flex; align-items: center; gap: 8px; }
	.recipient-row .hb-input { flex: 1; }
	.recipient-row .link {
		background: transparent; border: none; cursor: pointer; color: var(--accent);
		font: inherit; font-size: 11.5px; padding: 0; white-space: nowrap;
	}
	.recipient-row .link:hover { text-decoration: underline; }
</style>
