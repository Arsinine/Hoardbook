<script lang="ts">
	import { contacts, toast, toastWithAction, contactsLoadError, loadContactsInto } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import { refreshContact, importManifest, requestManifest, getManifestAsks, getContacts, groupsGet, groupsCreate, groupsCreateWithMembers, groupsAssign, groupsDelete, groupsUnassign, contactUpdateGroups, browsePrivateCollections, type ManifestAsk } from '$lib/api.js';
	import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import Avatar from '$lib/components/Avatar.svelte';
	import FeatureTooltip from '$lib/components/FeatureTooltip.svelte';
	// M22 W8 — ONE shared group-membership editor (the same component Contacts uses). Browse's
	// keyboard route for add/move/ungrouped opens this instead of a parallel, drifting copy.
	import GroupMembershipPopover from '$lib/components/GroupMembershipPopover.svelte';
	import CreateGroupDialog from '$lib/components/CreateGroupDialog.svelte';
	// QURATOR-98 — the shared dialog shell (backdrop, Escape, Tab trap, focus restore).
	import Modal from '$lib/components/Modal.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { collectionAvailability, peerAccessBadge, peerFromQuery, paywallTeaser, importedManifestNote, arrangeItems, fileTypesPresent, type BrowseViewMode, type BrowseSortKey, type BrowseSortDir } from '$lib/browse-view.js';
	import { deriveManifestAskState, ASK_TICK_MS, MANIFEST_ASKED_LINE, MANIFEST_ASK_AGAIN_LABEL, MANIFEST_ASK_AGAIN_COOLDOWN_TIP, MANIFEST_OPEN_CHAT_LABEL, MANIFEST_ASK_FAILED_LINE } from '$lib/manifest-ask.js';
	import type { CachedPeer, Collection, DirectoryItem, Group } from '$lib/types.js';
	import { groupByGroups, matchesQuery } from '$lib/contacts-view.js';
	// M22 W3 — drag-to-group gesture primitives (shared with Contacts). Create is ALWAYS ADDITIVE.
	// M22 W4 — drop onto an existing group heading: plain drop MOVES, Shift-drop ADDS (owner ruling
	// 2026-08-09). Ungrouped clears all. One implementation, two consumers (this + Contacts).
	// M22 W5 — multi-select drag primitives (shared with Contacts). Same selection model:
	// plain click selects one, Shift-click extends a contiguous run, Cmd/Ctrl toggles one.
	import { writeDragPayload, readDragPayload, writeDragPayloadMulti, readDragPayloadMulti, isValidDropTarget, isSelfDrop, groupSuggestions, groupSuggestionsMulti, commitCreateGroup, commitCreateGroupMulti, computeDropOutcome, commitDropOnGroup, computeDropOutcomeMulti, commitDropOnGroupMulti, applyClickToSelection, applyKeyToSelection, isTypingTargetShape, rovingTabindexForIdx, shouldHandleArrowKey, rowId, computeDropInverse, computeDropInverseMulti, computeCreateInverse, commitInverse, commitInverseMulti, UNGROUPED_TARGET, type DropOutcome, type DropOutcomeMulti } from '$lib/drag-group.js';
	import { contactDisplayName, shortNpub } from '$lib/contact-display.js';
	import { tick } from 'svelte';

	type BcItem =
		| { label: string; kind: 'contact' }
		| { label: string; kind: 'collection' }
		| { label: string; kind: 'folder'; index: number };

	let search = $state('');
	let selectedPeer = $state<CachedPeer | null>(null);
	let selectedCollection = $state<Collection | null>(null);
	let folderStack: { name: string; items: DirectoryItem[] }[] = $state([]);
	// devtest #3/#4: a keyed contact's collections are cached at add-time; a flaky fetch then leaves
	// them empty forever, so browsing shows nothing. Re-pull live when the peer is selected.
	let loadingListings = $state(false);

	// devtest v0.12.4 #4: file-view controls. viewMode/sort are sticky preferences; the type filter +
	// in-collection search reset on navigation (a stale filter would hide a folder's whole content).
	let viewMode: BrowseViewMode = $state('details');
	let sortKey: BrowseSortKey = $state('name');
	let sortDir: BrowseSortDir = $state('asc');
	let fileSearch = $state('');
	let activeTypes: string[] = $state([]);

	function resetFileFilters() {
		fileSearch = '';
		activeTypes = [];
	}

	function toggleType(t: string) {
		activeTypes = activeTypes.includes(t) ? activeTypes.filter((x) => x !== t) : [...activeTypes, t];
	}

	function peerName(peer: CachedPeer): string {
		// A legacy/adversarial teaser can carry display_name: "" (R1 only guards publish) — `??` would
		// not fall back to a literal empty string, showing a blank name; `||` does.
		return peer.profile?.display_name || peer.npub.slice(0, 10) + '…';
	}

	function peerInitial(peer: CachedPeer): string {
		return (peer.profile?.display_name?.[0] ?? peer.npub[0]).toUpperCase();
	}

	async function selectPeer(peer: CachedPeer) {
		selectedPeer = peer;
		selectedCollection = null;
		folderStack = [];
		resetFileFilters();
		// devtest #3/#4: for a keyed contact, re-fetch listings live so a browse-key that arrived
		// after (or a listing fetch that hiccuped at) add-time actually surfaces their collections.
		// Bare (keyless) contacts have nothing to fetch — skip. Cached view stays if the fetch fails.
		if (!peer.browse_key_hex) return;
		loadingListings = true;
		try {
			const updated = await refreshContact(peer.npub);
			contacts.update(cs => cs.map(c => c.npub === updated.npub ? { ...updated, local_tags: c.local_tags } : c));
			// Only replace the view if the user hasn't navigated to a different peer meanwhile.
			if (selectedPeer?.npub === updated.npub) selectedPeer = updated;
		} catch {
			/* keep the cached view — offline / relay hiccup shouldn't blank the panel */
		} finally {
			loadingListings = false;
		}
	}

	// M15 W4: resolve a `/browse?peer=<npub>` deep-link (from the Contacts "Browse" button) THROUGH
	// selectPeer, so the keyed-contact live-refetch (devtest #3/#4) fires by construction. Guarded so
	// it runs once per distinct param (and waits for contacts to load — peerFromQuery returns null
	// until the match exists).
	let lastDeepLinked = '';
	$effect(() => {
		const npub = $page.url.searchParams.get('peer') ?? '';
		if (!npub || npub === lastDeepLinked) return;
		const peer = peerFromQuery($page.url.searchParams, $contacts);
		if (peer) {
			lastDeepLinked = npub;
			selectPeer(peer);
		}
	});

	function selectCollection(col: Collection) {
		selectedCollection = col;
		folderStack = [];
		resetFileFilters();
	}

	// M16 W4: import a full-listing manifest the user received out of band, upgrading a truncated
	// paywall teaser to the whole tree. The backend verifies the manifest author against this peer's
	// npub before decrypting; on success the fade lifts (`truncated` cleared ⇒ `paywallTeaser` → null).
	let importingManifest = $state(false);
	let pasteOpen = $state(false);
	let pasteText = $state('');
	let askingOwner = $state(false);

	// M17 W7.1a — the ask must leave a trace. `request_manifest` delivers to the recipient's inbox only
	// (no self-copy), so without reading the persisted ask-trace map back the paywall block reads as
	// unchanged and the button as dead. The asked-state is derived PURELY from this map + the clock,
	// never from component-local state — so it survives a remount/restart. Re-read after every send
	// (success OR failure) so a rejected publish leaves the un-asked state (the record is written only
	// after `send_dm_inner` resolves; a failed ask must never render as "Asked").
	let manifestAsks = $state<Record<string, ManifestAsk> | null>(null);
	// A slow tick re-derives the cooldown countdown + the relative label so the tooltip and "Ask again"
	// disabled state stay honest without a page reload. Idempotent + cheap (pure derivation).
	let nowTick = $state(Date.now());
	let askError = $state<string | null>(null);

	async function refreshManifestAsks() {
		try {
			manifestAsks = await getManifestAsks();
		} catch {
			// A read failure must never blank a previously-rendered asked-state (the user DID ask). Keep
			// the stale map; the paywall block still renders the last-known asked-state from it.
		}
	}

	$effect(() => {
		// Load the ask map on mount so the asked-state renders from the persisted record immediately.
		refreshManifestAsks();
		// Tick to refresh the cooldown countdown + relative label. Both are MINUTE-granular
		// (`MANIFEST_ASK_AGAIN_COOLDOWN_TIP` ceils to minutes), so a 1s tick would wake the reactive
		// graph 60× per displayable change; ASK_TICK_MS bounds the lag at half a minute for a
		// fraction of the wakeups — same call W5 made for the contact-row clock (PRESENCE_TICK_MS).
		// The effect does NOT re-run on `nowTick`: it writes that state, never reads it, so the
		// interval is created once and torn down on unmount.
		const id = setInterval(() => { nowTick = Date.now(); }, ASK_TICK_MS);
		return () => clearInterval(id);
	});

	// M16 W4: the primary "get the rest" affordance — DM the owner asking for the full list. The owner
	// decides whether to export + ticket a manifest (Hoardbook never auto-produces one; MASCARA_SPEC Q1).
	async function handleAskOwner() {
		if (!selectedPeer || !selectedCollection) return;
		askingOwner = true;
		askError = null;
		try {
			await requestManifest(selectedPeer.npub, selectedCollection.slug, selectedCollection.snapshot_fingerprint ?? '', selectedCollection.teaser_event_id);
			toast('Asked the owner for the full list');
			// Re-read the persisted record so the asked-state renders from the store, not from optimistic
			// component state. The record is written server-side AFTER `send_dm_inner` resolves, so on
			// success it is guaranteed present; the catch branch handles the failure case explicitly.
			await refreshManifestAsks();
		} catch (e) {
			askError = String(e);
			toast(String(e), 'error');
			// "Failure is loud": leave the button in its un-asked state with the muted reason inline.
			// Re-read in case the store carries a prior successful ask (the user may be re-asking after
			// a cooldown and the previous asked-state should still show — not be hidden by this failure).
			await refreshManifestAsks();
		} finally {
			askingOwner = false;
		}
	}

	// Pure derivation of the paywall block's asked-state from (map, npub, slug, now). `nowTick` is the
	// reactive clock that keeps the cooldown + relative label fresh; reading it here makes the $derived
	// recompute every second.
	let askState = $derived(
		selectedPeer && selectedCollection
			? deriveManifestAskState(manifestAsks, selectedPeer.npub, selectedCollection.slug, new Date(nowTick))
			: { kind: 'unasked' as const },
	);

	async function handleImportManifest(source: { path?: string; pasted?: string }) {
		if (!selectedPeer || !selectedCollection) return;
		const targetNpub = selectedPeer.npub;
		const targetSlug = selectedCollection.slug;
		importingManifest = true;
		try {
			const result = await importManifest(targetNpub, targetSlug, source, selectedCollection.snapshot_fingerprint);
			const full = result.collection;
			// M19 W10: the await may have resolved after the user switched peers/collections. Only swap
			// the live view if they are still looking at the peer+collection the import was started
			// for — otherwise the result lands under a different peer's identity chrome (content
			// misattribution). Mirrors `selectPeer`'s `if (selectedPeer?.npub === updated.npub)` guard.
			if (selectedPeer?.npub === targetNpub && selectedCollection?.slug === targetSlug) {
				// Swap the truncated collection for the full tree, in the view and the in-memory contact.
				selectedPeer = {
					...selectedPeer,
					collections: selectedPeer.collections.map((c) => (c.slug === result.slug ? full : c)),
				};
				selectedCollection = full;
				folderStack = [];
			} else {
				// Stale result — fold the full tree into the background `contacts` store only, so a
				// later return to that peer shows the upgraded listing without clobbering whoever the
				// user is currently viewing.
				contacts.update((cs) =>
					cs.map((c) =>
						c.npub === targetNpub
							? { ...c, collections: c.collections.map((x) => (x.slug === result.slug ? full : x)) }
							: c,
					),
				);
			}
			if (result.stale) {
				toast('Imported an older version of this list — ask the owner for a fresh manifest.', 'error');
			} else {
				toast('Full manifest imported');
			}
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			importingManifest = false;
			pasteOpen = false;
			pasteText = '';
		}
	}

	async function pickManifestFile() {
		const path = await openFileDialog({
			multiple: false,
			filters: [{ name: 'Hoardbook manifest', extensions: ['hbmanifest'] }],
		});
		if (typeof path === 'string') await handleImportManifest({ path });
	}

	function enterFolder(item: DirectoryItem) {
		folderStack = [...folderStack, { name: item.name, items: item.children }];
		resetFileFilters();
	}

	function navigateBc(bc: BcItem) {
		if (bc.kind === 'contact') {
			selectedCollection = null;
			folderStack = [];
		} else if (bc.kind === 'collection') {
			folderStack = [];
		} else {
			folderStack = folderStack.slice(0, bc.index + 1);
		}
		resetFileFilters();
	}

	function fmtBytes(bytes: number): string {
		if (bytes > 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
		if (bytes > 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
		if (bytes > 1e3) return (bytes / 1e3).toFixed(0) + ' KB';
		return bytes + ' B';
	}

	// Build the relative path for a file within the collection.
	// devtest #9: no right-click "Copy path" — Hoardbook shows metadata only and moves no files, so
	// there's nothing to copy a usable path to; the context menu is removed entirely.

	// §6 Discovery moved to Contacts (devtest 2026-06-25 #6). Browse is now purely "browse a contact's
	// collections" — pick someone from the People list on the left.

	// M21 W5b: the People panel is grouped by the user's groups (reusing the Contacts view-model so
	// both tabs group identically). A contact in two groups appears under both; Ungrouped is a real
	// trailing section, never a bucket that hides anyone. The text filter now matches petname too
	// (it previously matched only display_name + npub — Contacts already matched petname).
	let groups: Group[] = $state([]);
	// Has the FIRST groupsGet() resolved? The membership editor must not commit a full-set replace
	// derived from an empty `groups` array — that would wipe the contact's memberships (Codex review
	// 2026-08-11). Passed to GroupMembershipPopover as `ready`.
	let groupsLoaded = $state(false);
	$effect(() => {
		// Load once on mount; group membership is mutated on the Contacts tab, and Browse re-reads
		// on every navigate here (the $effect re-runs when `groups` is reassigned elsewhere too).
		groupsGet().then((g) => { groups = g; groupsLoaded = true; }).catch(() => { /* non-fatal */ });
	});
	// QURATOR-92 — Private collections peers sealed TO US (M10), keyed by author npub. These are
	// the same decrypted Collections Contacts' card detail lists (inertly); here they render in a
	// badged Private section of the collections pane. A non-trusted viewer simply has no entry —
	// nothing about OUR audience (that is private_audience_*, untouched). Not merged into
	// selectedPeer.collections: the public count/grid stays public-only (M21 W4).
	let privateByAuthor: Record<string, Collection[]> = $state({});
	let selectedPrivate = $derived(selectedPeer ? (privateByAuthor[selectedPeer.npub] ?? []) : []);
	// QURATOR-93 (Browse half) — a FAILED browsePrivateCollections load used to be swallowed by the
	// bare `.catch(() => {})` below, so the Private section just never appeared: indistinguishable
	// from a peer who genuinely has none. page-local (this fetch isn't keyed to one peer), cleared on
	// a later success (both-directions rule, same as contactsLoadError/collectionsLoadError).
	let privateLoadError = $state(false);
	async function loadPrivateInto() {
		try {
			const list = await browsePrivateCollections();
			const map: Record<string, Collection[]> = {};
			for (const g of list) map[g.npub] = g.collections;
			privateByAuthor = map;
			privateLoadError = false;
		} catch {
			privateLoadError = true;
		}
	}
	$effect(() => {
		// Load once on mount, mirroring the groups load above.
		void loadPrivateInto();
	});
	let filteredContacts = $derived($contacts.filter(p => matchesQuery(p, search)));
	let peopleSections = $derived(groupByGroups(filteredContacts, groups));
	let currentItems = $derived(folderStack.length > 0
		? folderStack[folderStack.length - 1].items
		: (selectedCollection?.listing ?? []));
	// devtest v0.12.4 #4: the distinct file types in the current folder feed the filter chips; the
	// arranged list applies search + type filter + sort (folders-first) — all in the tested seam.
	let availableTypes = $derived(fileTypesPresent(currentItems));
	let sortedItems = $derived(arrangeItems(currentItems, { search: fileSearch, types: activeTypes, sortKey, sortDir }));
	// devtest #7 / M16 W3: a peer's collection published as a truncated paywall teaser (too large to
	// publish whole). Shown only at the collection root, where the dropped tail makes the fade honest;
	// a collection the browser upgraded to the full tree from a big relay has `truncated` cleared, so
	// `paywallTeaser` returns null and the full tree renders (no fade).
	let paywall = $derived(folderStack.length > 0 ? null : paywallTeaser(selectedCollection));
	let breadcrumbs = $derived<BcItem[]>([
		...(selectedPeer ? [{ label: peerName(selectedPeer), kind: 'contact' as const }] : []),
		...(selectedCollection ? [{ label: selectedCollection.path_alias, kind: 'collection' as const }] : []),
		...folderStack.map((f, i) => ({ label: f.name, kind: 'folder' as const, index: i })),
	]);
	// Feature-tooltip anchor data (HOARDBOOK_SPEC §8).
	let peerWillingTo = $derived(selectedPeer?.profile?.willing_to ?? []);
	// A peer followed by bare npub (no share code) has sealed listings — they can't be decrypted.
	let listingsLocked = $derived(!!selectedPeer && !selectedPeer.browse_key_hex && selectedPeer.collections.length === 0);

	// M22 W3 — drag-to-group gesture on the People list. Same shared primitives as Contacts;
	// create is ALWAYS ADDITIVE. Esc cancels. The naming popover is a simple inline panel here
	// (Browse's People list is compact, so a full OverflowMenu is overkill).
	//
	// M22 W5 — multi-select: same model as Contacts (plain/shift/cmd-click). Dragging a selected
	// row carries the whole selection; dragging an unselected row carries just that row.
	let selectedNpubs = $state<string[]>([]);
	let selectionAnchor = $state<string | null>(null);
	let selectedNpubSet = $derived(new Set(selectedNpubs));
	// M22 W7 — the keyboard-focused row (roving tabindex). Arrow keys move it; with Shift they
	// extend the selection. focusedIdx is the RENDER index, not a per-npub lookup: peopleSections
	// renders a peer once per group, so keying the roving tabindex on npub alone would make every
	// copy of a multi-group contact tabbable (A5).
	let focusedNpub = $state<string | null>(null);
	let focusedIdx = $state<number | null>(null);
	let contactOrder = $derived(peopleSections.flatMap((s) => s.peers).map((p) => p.npub));
	// A2/A7 — the list container, so arrow keys are only handled when focus is actually inside it.
	let listContainer: HTMLElement | undefined = $state();
	const ROW_ID_PREFIX = 'browse-peer-row';

	// M22 W7 A5 — the GLOBAL render index each section starts at. groupByGroups renders a peer once
	// per group, so a roving tabindex keyed on npub alone would mark every copy tabbable.
	let sectionStart = $derived((() => {
		const out: number[] = [];
		let n = 0;
		for (const sec of peopleSections) { out.push(n); n += sec.peers.length; }
		return out;
	})());

	function onPeerMouseDown(e: MouseEvent, npub: string) {
		// Ignore clicks on inner controls (avatar lock, etc.) so they keep their own action.
		if ((e.target as HTMLElement).closest('button, a')) return;
		const r = applyClickToSelection(selectedNpubs, selectionAnchor, contactOrder, npub, e.shiftKey, e.metaKey || e.ctrlKey);
		selectedNpubs = r.selection;
		selectionAnchor = r.anchor;
		// M22 W7 — a mouse click also moves focus so a subsequent arrow key continues from here.
		focusedNpub = npub;
		focusedIdx = contactOrder.indexOf(npub);
	}

	// M22 W7 A2 — when a row receives focus (by Tab or click), record it so Tab-then-Arrow does
	// not restart from the nearest end.
	function onRowFocus(npub: string) {
		if (focusedNpub !== npub) {
			focusedNpub = npub;
			focusedIdx = contactOrder.indexOf(npub);
		}
	}

	// M22 W7 — the keyboard equivalent of the drag-to-group gesture (mirrors Contacts). ArrowUp/Down
	// move focus; Shift extends the selection; G opens the SAME namer; Esc clears. Guarded against
	// eating typing (input/textarea/select/contenteditable, the namer, the W5b popover, modifiers).
	function isTypingTarget(e: KeyboardEvent): boolean {
		// A8: a window keydown retargets to the shadow HOST for events from inside a Shadow DOM;
		// composedPath()[0] is the actual inner element. Combined with the pure isTypingTargetShape
		// so the guard logic is unit-testable without a DOM.
		const t = (e.composedPath()[0] ?? e.target) as HTMLElement | null;
		if (!(t instanceof HTMLElement)) return false;
		return isTypingTargetShape(t.tagName, t.isContentEditable);
	}

	// M22 W7 A7 — true when focus is within the list container (so arrow keys should be handled
	// and preventDefault'd) OR a row is explicitly focused.
	function listHasFocus(): boolean {
		if (focusedNpub !== null) return true;
		if (listContainer && document.activeElement && listContainer.contains(document.activeElement)) return true;
		return false;
	}

	async function moveFocusToRow(npub: string) {
		// A2: move real DOM focus to the newly-focused row after the DOM has updated. Without this
		// only tabindex changes, so assistive tech never follows the arrow keys.
		await tick();
		document.getElementById(rowId(ROW_ID_PREFIX, npub))?.focus();
	}

	function onWindowKeyDown(e: KeyboardEvent) {
		// The namer handles its own keys; don't compete.
		if (dragPopoverFor) return;
		// M22 W8 — when the SHARED group-membership editor is open, it owns the keyboard (its
		// checkboxes take arrows/tab natively; OverflowMenu owns Escape). Don't move the list
		// selection underneath an open editor.
		if (groupPopoverFor) return;
		// A7: only handle/preventDefault arrows when the list actually has focus, so they still
		// scroll the page when the user is elsewhere.
		if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !isTypingTarget(e) && listHasFocus()) {
			if (!shouldHandleArrowKey(contactOrder.length)) return;
			e.preventDefault();
			const r = applyKeyToSelection(selectedNpubs, selectionAnchor, contactOrder, focusedNpub, e.key, e.shiftKey);
			selectedNpubs = r.selection;
			selectionAnchor = r.anchor;
			focusedNpub = r.focused;
			focusedIdx = r.focusedIdx;
			void moveFocusToRow(r.focused);
			return;
		}
		if (e.key === 'Escape' && selectedNpubs.length > 0) {
			selectedNpubs = [];
			selectionAnchor = null;
			return;
		}
		if ((e.key === 'g' || e.key === 'G') && !isTypingTarget(e) && !e.ctrlKey && !e.metaKey && !e.altKey && selectedNpubs.length >= 2) {
			e.preventDefault();
			const peers = selectedNpubs
				.map((n) => $contacts.find((c) => c.npub === n))
				.filter((p): p is CachedPeer => !!p);
			if (peers.length < 2) return;
			// MUST be set before dragPopoverFor, so the success path can restore it too.
			dragPopoverReturnFocus = document.activeElement as HTMLElement | undefined;
			dragSuggestions = groupSuggestionsMulti(peers);
			dragNameInput = '';
			dragPopoverFor = [...selectedNpubs];
			focusNamer();
		}
		// M22 W8 — E opens the SHARED group-membership editor for the focused row: the keyboard
		// route for add/move/ungrouped. Same component + same contactUpdateGroups full-set write as
		// Contacts, so the two surfaces cannot drift. The editor is never reachable while typing.
		if ((e.key === 'e' || e.key === 'E') && !isTypingTarget(e) && !e.ctrlKey && !e.metaKey && !e.altKey && focusedNpub) {
			e.preventDefault();
			// MUST be set before openGroupPopover so the success/cancel path can restore it.
			dragPopoverReturnFocus = document.activeElement as HTMLElement | undefined;
			openGroupPopover(focusedNpub);
		}
	}

	let dragSourceNpub = $state<string | null>(null);
	let dragOverNpub = $state<string | null>(null);
	// M22 W5: how many contacts are being dragged (0 for single-pair W3, N-1 for multi of N).
	let dragCount = $state(0);
	// M22 W6: prior group membership captured at drag START (not drop) so a slow write between
	// start and drop can't race the inverse. Keyed by npub; emptied on dragend.
	let priorGroupsByNpub = $state<Map<string, string[]>>(new Map());
	let dragPopoverFor = $state<string[] | { source: string; target: string } | null>(null);
	let dragNameInput = $state('');
	// M22 W7 A3 — the namer's input element, so opening the namer can focus it.
	let dragNameEl: HTMLInputElement | undefined = $state();
	let dragSuggestions = $state<string[]>([]);
	// M22 W7 — the row that had focus before the namer opened, so focus returns to it on close.
	let dragPopoverReturnFocus: HTMLElement | undefined = $state();

	// M22 W8 — the SHARED group-membership editor. Browse's keyboard route for add/move/ungrouped.
	// Same component + same full-set write (contactUpdateGroups) as Contacts, so the two surfaces
	// cannot drift. Never touches the private audience.
	let groupPopoverFor: string | null = $state(null);
	let groupPopoverAnchor: HTMLElement | undefined = $state();
	let createGroupOpen = $state(false);
	// The contact the editor is showing (derived from the focused npub; absent only while closed).
	let gmpPeer = $derived(groupPopoverFor ? $contacts.find((c) => c.npub === groupPopoverFor) : undefined);
	let gmpContactName = $derived(gmpPeer ? contactDisplayName(gmpPeer) : (groupPopoverFor ? shortNpub(groupPopoverFor) : ''));
	let gmpMemberships = $derived(groupPopoverFor ? peerGroupsOf(groupPopoverFor) : []);

	async function handleCreateGroup(detail: { name: string; color: string }) {
		const { name, color } = detail;
		try {
			await groupsCreate(name, color);
			await loadGroupsInto();
			createGroupOpen = false;
			toast(`Group "${name}" created`);
		} catch (e) { toast(String(e), 'error'); }
	}

	// Open the shared editor anchored to the focused row (the keyboard route) — same full-set
	// semantics as Contacts: the draft is seeded from current memberships, Apply sends the whole
	// checked set through contactUpdateGroups.
	// Captured at open and NOT cleared by onclose, so focus restoration still knows which row to go
	// back to after groupPopoverFor has been nulled (Codex review 2026-08-11).
	let gmpOpenedFor: string | null = $state(null);

	function openGroupPopover(npub: string) {
		const row = document.getElementById(rowId(ROW_ID_PREFIX, npub));
		groupPopoverAnchor = row ?? undefined;
		groupPopoverFor = npub;
		gmpOpenedFor = npub;
	}

	async function applyGroupPopover(npub: string, names: string[]) {
		groupPopoverFor = null;
		try {
			await contactUpdateGroups(npub, names);
			await loadGroupsInto();
		} catch (e) { toast(String(e), 'error'); }
	}

	async function loadGroupsInto() {
		try { groups = await groupsGet(); groupsLoaded = true; } catch { /* non-fatal */ }
	}

	// QURATOR-93 (Browse half) — the People rail's Retry affordance. Same helper + same store as
	// Contacts (`loadContactsInto`), so a successful retry here also clears contactsLoadError for
	// Contacts (both read the one shared store).
	async function retryContactsLoad() {
		await loadContactsInto(getContacts);
	}

	function onDragStart(e: DragEvent, npub: string) {
		if (!e.dataTransfer) return;
		let carried: string[];
		// M22 W5: carry the whole selection when dragging a selected row; just this row otherwise.
		if (selectedNpubSet.has(npub) && selectedNpubs.length > 1) {
			writeDragPayloadMulti(e.dataTransfer, selectedNpubs);
			dragCount = selectedNpubs.length;
			carried = [...selectedNpubs];
		} else {
			selectedNpubs = [npub];
			selectionAnchor = npub;
			writeDragPayload(e.dataTransfer, npub);
			dragCount = 0;
			carried = [npub];
		}
		dragSourceNpub = npub;
		// M22 W6: capture prior membership at drag START for the move inverse.
		const m = new Map<string, string[]>();
		for (const n of carried) m.set(n, peerGroupsOf(n));
		priorGroupsByNpub = m;
	}

	function onDragOver(e: DragEvent, npub: string) {
		if (!dragSourceNpub) return;
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
		dragOverNpub = npub;
	}

	function onDragLeave(npub: string) {
		if (dragOverNpub === npub) dragOverNpub = null;
	}

	function onDragEnd() {
		dragSourceNpub = null;
		dragOverNpub = null;
		dragCount = 0;
		// M22 W4: clear the group-heading drop affordance state too.
		dropOverTarget = null;
		dropOutcome = null;
		dropOutcomeMulti = null;
		priorGroupsByNpub = new Map();
	}

	function onDrop(e: DragEvent, targetNpub: string) {
		e.preventDefault();
		// M22 W5: multi-select drop — open naming popover with all selected peers + target.
		const multiNpubs = readDragPayloadMulti(e.dataTransfer);
		if (multiNpubs && multiNpubs.length > 1) {
			if (multiNpubs.includes(targetNpub)) {
				dragSourceNpub = null;
				dragOverNpub = null;
				return;
			}
			const peers = [...multiNpubs, targetNpub]
				.map((n) => $contacts.find((c) => c.npub === n))
				.filter((p): p is CachedPeer => !!p);
			if (peers.length < 2) return;
			dragSuggestions = groupSuggestionsMulti(peers);
			dragNameInput = '';
			dragPopoverFor = [...multiNpubs, targetNpub];
			focusNamer();
			dragOverNpub = null;
			return;
		}
		const sourceNpub = readDragPayload(e.dataTransfer);
		if (!sourceNpub || isSelfDrop(sourceNpub, targetNpub)) {
			dragSourceNpub = null;
			dragOverNpub = null;
			return;
		}
		if (!isValidDropTarget(sourceNpub, targetNpub)) return;
		const source = $contacts.find((c) => c.npub === sourceNpub);
		const target = $contacts.find((c) => c.npub === targetNpub);
		if (!source || !target) return;
		dragSuggestions = groupSuggestions(source, target);
		dragNameInput = '';
		dragPopoverFor = { source: sourceNpub, target: targetNpub };
		focusNamer();
		dragOverNpub = null;
	}

	function closeDragPopover() {
		dragPopoverFor = null;
		dragNameInput = '';
		dragSuggestions = [];
		// M22 W7 — return focus to the row that opened the namer. A9: .focus() on a DETACHED node is
		// a silent no-op (the row may have been filtered away while the namer was open), so check
		// isConnected and fall back to the first rendered row rather than stranding focus on <body>.
		if (dragPopoverReturnFocus?.isConnected) {
			dragPopoverReturnFocus.focus();
		} else if (contactOrder.length > 0) {
			document.getElementById(rowId(ROW_ID_PREFIX, contactOrder[0]))?.focus();
		}
		dragPopoverReturnFocus = undefined;
	}

	// M22 W7 A3 — focus the namer's field once it has rendered. Without this the popover opens and
	// typing goes nowhere: the window handler ignores keys while the namer is open, so the gesture
	// dead-ends. Awaits tick() because the input does not exist until the {#if} block renders.
	async function focusNamer() {
		await tick();
		dragNameEl?.focus();
	}

	async function onDragNameKey(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			await commitDragCreate();
		} else if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			closeDragPopover();
		}
	}

	async function commitDragCreate() {
		if (!dragPopoverFor) return;
		const npubs = dragPopoverFor;
		const name = dragNameInput;
		// A4: close through closeDragPopover so focus is restored on the SUCCESS path too — setting
		// dragPopoverFor = null directly here restored focus only on cancel.
		closeDragPopover();
		try {
			if (Array.isArray(npubs)) {
				// M22 W5 multi-select create: ONE call with all N npubs.
				await commitCreateGroupMulti({ groupsCreateWithMembers }, name, npubs, groups);
			} else {
				await commitCreateGroup({ groupsCreateWithMembers }, name, npubs.source, npubs.target, groups);
			}
			await loadGroupsInto();
			// M22 W6: create's inverse is delete (the group was brand new, so safe).
			toastWithAction(`Group "${name.trim()}" created`, {
				label: 'Undo',
				run: () => {
					commitInverse({ groupsDelete, groupsUnassign, contactUpdateGroups }, computeCreateInverse(name.trim()))
						.then(() => loadGroupsInto())
						.catch((e) => toast(String(e), 'error'));
				},
			});
			// M22 W5: clear the selection after a successful create.
			selectedNpubs = [];
			selectionAnchor = null;
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			dragNameInput = '';
			dragSuggestions = [];
		}
	}

	// M22 W4 — drop onto an existing group heading (mirrors Contacts' onGroupDragOver/Leave/Drop;
	// one implementation, two consumers). Plain drop MOVES; Shift-drop ADDS (owner ruling
	// 2026-08-09). Refused before release. Ungrouped clears all — no confirm, no undo. This path
	// never touches the private audience.
	let dropOverTarget: string | null = $state(null);
	let dropOutcome: DropOutcome | null = $state(null);
	// M22 W5: the multi-select affordance (parallel to dropOutcome for the single-source case).
	let dropOutcomeMulti: DropOutcomeMulti | null = $state(null);

	// M22 W6: show a toast with an Undo button. Local-only — no relay traffic.
	// M22 W6: the toast names the affected contact, not just the group. Delegates to the canonical
	// contactDisplayName (petname → display_name → shortNpub) rather than re-deriving the fallback,
	// so a toast can never name a contact differently from the row the user just dragged.
	function dropName(npub: string): string {
		const c = $contacts.find((p) => p.npub === npub);
		return c ? contactDisplayName(c) : shortNpub(npub);
	}

	function registerUndo(label: string, inverses: import('$lib/drag-group.js').DropInverse[]) {
		toastWithAction(label, {
			label: 'Undo',
			run: () => {
				commitInverseMulti({ groupsDelete, groupsUnassign, contactUpdateGroups }, inverses)
					.then(() => loadGroupsInto())
					.catch((e) => toast(String(e), 'error'));
			},
		});
	}

	function peerGroupsOf(npub: string): string[] {
		return groups.filter(g => g.pubkeys.includes(npub)).map(g => g.name);
	}

	function onGroupDragOver(e: DragEvent, targetName: string) {
		// M22 W5: read multi payload first; fall back to single-npub.
		const multiNpubs = readDragPayloadMulti(e.dataTransfer);
		if (multiNpubs && multiNpubs.length > 1) {
			e.preventDefault();
			const groupsByNpub = new Map(multiNpubs.map((n) => [n, peerGroupsOf(n)]));
			const outcome = computeDropOutcomeMulti(multiNpubs, targetName, groupsByNpub, e.shiftKey);
			dropOverTarget = targetName;
			dropOutcomeMulti = outcome;
			if (e.dataTransfer) {
				e.dataTransfer.dropEffect = (outcome.kind === 'refused' || outcome.kind === 'noop') ? 'none' : (e.shiftKey ? 'copy' : 'move');
			}
			return;
		}
		const sourceNpub = readDragPayload(e.dataTransfer);
		if (!sourceNpub) return;
		e.preventDefault();
		const outcome = computeDropOutcome(sourceNpub, targetName, peerGroupsOf(sourceNpub), e.shiftKey);
		dropOverTarget = targetName;
		dropOutcome = outcome;
		if (e.dataTransfer) {
			e.dataTransfer.dropEffect = (outcome.kind === 'refused' || outcome.kind === 'noop') ? 'none' : (e.shiftKey ? 'copy' : 'move');
		}
	}

	function onGroupDragLeave(targetName: string) {
		if (dropOverTarget === targetName) {
			dropOverTarget = null;
			dropOutcome = null;
			dropOutcomeMulti = null;
		}
	}

	async function onGroupDrop(e: DragEvent, targetName: string) {
		// M22 W5: multi-select drop onto a group.
		const multiNpubs = readDragPayloadMulti(e.dataTransfer);
		if (multiNpubs && multiNpubs.length > 1) {
			e.preventDefault();
			const groupsByNpub = new Map(multiNpubs.map((n) => [n, peerGroupsOf(n)]));
			const outcome = (dropOutcomeMulti ?? computeDropOutcomeMulti(multiNpubs, targetName, groupsByNpub, e.shiftKey));
			if (outcome.kind === 'refused' || outcome.kind === 'noop') {
				dropOverTarget = null;
				dropOutcome = null;
				dropOutcomeMulti = null;
				return;
			}
			dropOverTarget = null;
			dropOutcome = null;
			dropOutcomeMulti = null;
			try {
				const committed = await commitDropOnGroupMulti(
					{ groupsAssign, contactUpdateGroups },
					outcome,
				);
				await loadGroupsInto();
				// M22 W6: register the inverse (null for ungrouped, by owner ruling).
				const inverses = computeDropInverseMulti(committed, priorGroupsByNpub);
				if (committed.kind === 'add') {
					const label = `Added ${committed.npubs.length} to ${committed.target}`;
					if (inverses) registerUndo(label, inverses);
					else toast(label);
				} else if (committed.kind === 'move') {
					const label = `Moved ${committed.npubs.length} to ${committed.target}`;
					if (inverses) registerUndo(label, inverses);
					else toast(label);
				} else {
					// ungrouped: NO inverse, by owner ruling ("This isn't a word doc.").
					toast(`Moved ${committed.npubs.length} to Ungrouped`);
				}
				selectedNpubs = [];
				selectionAnchor = null;
			} catch (e) {
				toast(String(e), 'error');
			}
			return;
		}
		const sourceNpub = readDragPayload(e.dataTransfer);
		if (!sourceNpub) return;
		e.preventDefault();
		const outcome = dropOutcome ?? computeDropOutcome(sourceNpub, targetName, peerGroupsOf(sourceNpub), e.shiftKey);
		if (outcome.kind === 'refused' || outcome.kind === 'noop') {
			dropOverTarget = null;
			dropOutcome = null;
			return;
		}
		dropOverTarget = null;
		dropOutcome = null;
		try {
			const committed = await commitDropOnGroup(
				{ groupsAssign, contactUpdateGroups },
				sourceNpub,
				outcome,
			);
			await loadGroupsInto();
			// M22 W6: register the inverse (null for ungrouped, by owner ruling).
			const prior = priorGroupsByNpub.get(sourceNpub) ?? [];
			const inverse = computeDropInverse(sourceNpub, committed, prior);
			if (committed.kind === 'add') {
				const label = `Added ${dropName(sourceNpub)} to ${committed.target}`;
				if (inverse) registerUndo(label, [inverse]);
				else toast(label);
			} else if (committed.kind === 'move') {
				const label = `Moved ${dropName(sourceNpub)} to ${committed.target}`;
				if (inverse) registerUndo(label, [inverse]);
				else toast(label);
			} else {
				// ungrouped: NO inverse, by owner ruling ("This isn't a word doc.").
				toast(`Moved ${dropName(sourceNpub)} to Ungrouped`);
			}
		} catch (e) {
			toast(String(e), 'error');
		}
	}
</script>

<!-- M22 W5 — Esc clears the multi-selection when the naming popover is not open. -->
<!-- M22 W7 — the live region mirrors the drag affordance text so the refuse state is conveyed
     non-visually, not just by withholding the accent highlight. -->
<svelte:window onkeydown={onWindowKeyDown} />
<div class="sr-only" role="status" aria-live="polite">
	{#if dropOverTarget && dropOutcome}
		{#if dropOutcome.kind === 'refused'}{dropOutcome.reason}{:else if dropOutcome.kind === 'noop'}already ungrouped{:else if dropOutcome.kind === 'add'}add to {dropOverTarget}{:else if dropOutcome.kind === 'move'}move to {dropOverTarget}{:else}remove all groups{/if}
	{/if}
</div>

<div class="browse-shell">
	<!-- Left: contact list -->
	<div class="left-panel">
		<!-- QURATOR-81: Browse has no unconditional right-panel header (its content is
		     peer-selection-dependent), so panel-top is the one always-present bar that can carry a
		     drag region. Left panel, clear of the window controls in the top-right — no padding
		     reservation needed. -->
		<div class="panel-top" data-tauri-drag-region>
			<span class="panel-title">People</span>
		</div>
		<div class="hb-input search-wrap">
			<span class="search-icon">{@html icons.search}</span>
			<input class="search-input" placeholder="Filter contacts…" bind:value={search} />
		</div>

		<div class="contact-list" bind:this={listContainer}>
			{#if $contactsLoadError}
				<!-- QURATOR-93 (Browse half): a FAILED contacts load must not render as the confident
				     "No contacts yet" negative — that string is indistinguishable from a genuine empty. -->
				<EmptyState
					error
					message="Couldn't load contacts — the peer cache didn't answer."
					onretry={retryContactsLoad}
				/>
			{:else if $contacts.length === 0}
				<div class="left-empty">No contacts yet</div>
			{:else if filteredContacts.length === 0}
				<div class="left-empty">No matches</div>
			{:else}
				{#each peopleSections as section, secIdx (section.key)}
					{@const secGroup = section.key === 'ungrouped' ? null : groups.find(g => g.name === section.key)}
					{@const dropTargetName = section.key === 'ungrouped' ? UNGROUPED_TARGET : section.key}
					<div class="people-section">
						<!-- M22 W4: the section head is a drop target (plain move / Shift add / Ungrouped clears). -->
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div
							class="people-section-head"
							class:group-drop-active={dropOverTarget === dropTargetName && dropOutcome && dropOutcome.kind !== 'refused' && dropOutcome.kind !== 'noop'}
							class:group-drop-refused={dropOverTarget === dropTargetName && dropOutcome && (dropOutcome.kind === 'refused' || dropOutcome.kind === 'noop')}
							ondragover={(e) => onGroupDragOver(e, dropTargetName)}
							ondragleave={() => onGroupDragLeave(dropTargetName)}
							ondrop={(e) => onGroupDrop(e, dropTargetName)}
							role="group"
							aria-label={section.label}
							aria-disabled={dropOverTarget === dropTargetName && dropOutcome !== null && (dropOutcome.kind === 'refused' || dropOutcome.kind === 'noop')}
						>
							{#if secGroup?.color}
								<span class="people-group-dot" style={`background:${secGroup.color}`}></span>
							{/if}
							<span class="people-section-title">{section.label}</span>
							<span class="people-section-count">{section.peers.length}</span>
							{#if dropOverTarget === dropTargetName && dropOutcome}
								<span class="drop-hint-browse">{#if dropOutcome.kind === 'refused'}{dropOutcome.reason}{:else if dropOutcome.kind === 'noop'}already ungrouped{:else if dropOutcome.kind === 'add'}⇧ add{:else if dropOutcome.kind === 'move'}move{:else}clear all{/if}</span>
							{/if}
						</div>
						{#each section.peers as peer, i (peer.npub)}
							{@const letter = peerInitial(peer)}
							{@const hue = avatarHue(letter)}
							{@const badge = peerAccessBadge(peer)}
							<!-- M22 W3: each People row is a drag source AND drop target for drag-to-group. -->
							<!-- M22 W5: peer-selected marks the multi-drag selection (distinct from
							     contact-selected which means "shown in the right panel"). -->
							<!-- M22 W7 — roving tabindex: one row owns tabindex=0 (the focused one, or the
							     first when none is focused yet); the rest carry -1 so arrow keys move a single
							     tab stop. The aria-label carries selection state for assistive tech. -->
							<!-- svelte-ignore a11y_no_static_element_interactions -->
							<div
								class="contact-row"
								class:contact-selected={selectedPeer?.npub === peer.npub}
								class:peer-selected={selectedNpubSet.has(peer.npub)}
								class:drag-source={dragSourceNpub === peer.npub}
								class:drag-target={dragSourceNpub !== null && dragOverNpub === peer.npub && dragSourceNpub !== peer.npub}
								draggable="true"
								onmousedown={(e) => onPeerMouseDown(e, peer.npub)}
								ondragstart={(e) => onDragStart(e, peer.npub)}
								ondragover={(e) => onDragOver(e, peer.npub)}
								ondragleave={() => onDragLeave(peer.npub)}
								ondrop={(e) => onDrop(e, peer.npub)}
								ondragend={onDragEnd}
								onclick={() => selectPeer(peer)}
								onkeydown={(e) => e.key === 'Enter' && selectPeer(peer)}
								role="button"
								aria-label={peerName(peer) + (selectedNpubSet.has(peer.npub) ? ', selected' : '')}
								id={rowId(ROW_ID_PREFIX, peer.npub)}
								tabindex={rovingTabindexForIdx(focusedIdx, sectionStart[secIdx] + i, contactOrder.length)}
								onfocus={() => onRowFocus(peer.npub)}
							>
								<div class="avatar-wrap">
									<Avatar {letter} size={24} {hue} picture={peer.profile?.picture} />
									<!-- devtest v0.12.1 #3: the browse-key lock/unlock icon overlays the avatar's top-right
									     (the online dot owns the bottom-right); the inline text badge is gone. -->
									<span class="access-lock" class:locked={badge.locked} title={badge.hint || badge.label}>{badge.icon}</span>
									{#if peer.online}
										<span class="online-dot"></span>
									{/if}
								</div>
								<span class="contact-name">{peerName(peer)}</span>
								{#if dragSourceNpub !== null && dragOverNpub === peer.npub && dragSourceNpub !== peer.npub}
									<span class="drag-outcome-browse">{dragCount > 0 ? `group ${dragCount + 1} contacts` : 'group these two'}</span>
								{:else}
									<span class="contact-meta">{peer.collections.length}</span>
								{/if}
							</div>
						{/each}
					</div>
				{/each}
			{/if}
		</div>
	</div>

	<!-- Right: browser -->
	<div class="right-panel">
		{#if !selectedPeer}
			<!-- Browse = view a contact's collections. Finding/adding people (lookup + Discover
			     hoarders) now lives on Contacts (devtest 2026-06-25 #6). -->
			<div class="empty-state">
				<div class="empty-icon">{@html icons.folder}</div>
				<div class="empty-label">Select a contact to browse their collections</div>
				{#if $contacts.length === 0}
					<a class="empty-cta" href="/contacts">Find hoarders in Contacts →</a>
				{/if}
			</div>
		{:else}
			<!-- Breadcrumb -->
			<div class="breadcrumb">
				{#each breadcrumbs as bc, i}
					{#if i > 0}
						<span class="bc-sep">{@html icons.chevronRight}</span>
					{/if}
					{#if i < breadcrumbs.length - 1}
						<button class="bc-btn" onclick={() => navigateBc(bc)}>{bc.label}</button>
					{:else}
						<span class="bc-current">{bc.label}</span>
					{/if}
				{/each}
			</div>

			<!-- Willing-to hints for the selected peer (off-platform exchange preferences) -->
			{#if !selectedCollection && peerWillingTo.length > 0}
				<div class="willing-bar">
					<span class="willing-label">
						Willing to<FeatureTooltip key="willing-to" />
					</span>
					{#each peerWillingTo as w}
						<span class="willing-chip">{w}</span>
					{/each}
				</div>
			{/if}

			<!-- Collections grid -->
			{#if !selectedCollection}
				{#if loadingListings && selectedPeer.collections.length === 0}
					<div class="empty-state">
						<div class="empty-icon">{@html icons.folder}</div>
						<div class="empty-label">Loading collections…</div>
					</div>
				{:else if listingsLocked}
					<div class="empty-state">
						<div class="empty-icon">{@html icons.folder}</div>
						<div class="empty-label">
							🔒 Listings locked<FeatureTooltip key="listings-locked" />
						</div>
						<!-- M17 W2: turn the locked dead-end into a next step → ask-access deep-link (a
						     prefilled DM draft, no wire change). selectedPeer is a CachedPeer (has petname);
						     guarded because the listingsLocked derivation's non-null narrowing doesn't reach
						     this closure. -->
						<button class="btn-default btn-sm ask-access-btn" onclick={() => { const p = selectedPeer; if (!p) return; goto('/chat?peer=' + p.npub + '&intent=ask-access' + (p.petname ? '&petname=' + encodeURIComponent(p.petname) : '')); }}>Ask for access</button>
					</div>
				{:else if selectedPeer.collections.length === 0}
					{@const p = selectedPeer}
					<EmptyState
						centered
						icon={icons.folder}
						message="No public collections"
						cta={{ label: 'Ask for access →', href: '/chat?peer=' + p.npub + '&intent=ask-access' + (p.petname ? '&petname=' + encodeURIComponent(p.petname) : '') }}
					/>
				{:else}
					<div class="col-grid">
						{#each selectedPeer.collections as col (col.slug)}
							<button class="col-card" onclick={() => selectCollection(col)}>
								<div class="col-card-icon">{@html icons.folder}</div>
								<div class="col-card-name">{col.path_alias}</div>
								{#if col.description}
									<div class="col-card-desc">{col.description}</div>
								{/if}
								<div class="col-card-meta">
									{col.item_count} item{col.item_count !== 1 ? 's' : ''}
									{#if col.est_size}· {col.est_size}{:else if col.total_bytes}· {fmtBytes(col.total_bytes)}{/if}
								</div>
								{#if (col.content_types?.length ?? 0) > 0 || col.sorted}
									<div class="col-tags">
										{#each (col.content_types ?? []).slice(0, 3) as t}
											<span class="tag">{t}</span>
										{/each}
										{#if col.sorted}
											<span class="tag tag-sorted">sorted</span>
										{/if}
									</div>
								{/if}
							</button>
						{/each}
					</div>
				{/if}

				<!-- QURATOR-92 — collections the peer sealed TO US (M10), rendered through the same
				     col-card machinery but badged Private and kept OUT of the public grid above (the
				     M21 W4 boundary: private never inflates the public count or grid). Absent for a
				     non-trusted viewer — no locked-teaser hint.
				     QURATOR-93 (Browse half) — a FAILED load used to be silently swallowed (see
				     loadPrivateInto above), so this section just never appeared: indistinguishable
				     from a peer with none. It now also opens on privateLoadError, rendering a
				     retryable error line instead of staying silently absent; genuine empties (no
				     error, zero private collections) still render nothing, unchanged. -->
				{#if !selectedCollection && (selectedPrivate.length > 0 || privateLoadError)}
					<div class="private-collections">
						<div class="private-collections-label">
							Private collections
							<span class="private-pill" title="Sealed to you by the owner — not visible to other viewers.">Private</span>
						</div>
						{#if privateLoadError}
							<EmptyState error message="Couldn't load private collections." onretry={loadPrivateInto} />
						{:else}
							<div class="col-grid">
								{#each selectedPrivate as col (col.slug)}
									<button class="col-card" onclick={() => selectCollection(col)}>
										<div class="col-card-icon">{@html icons.folder}</div>
										<div class="col-card-name">{col.path_alias}</div>
										{#if col.description}
											<div class="col-card-desc">{col.description}</div>
										{/if}
										<div class="col-card-meta">
											{col.item_count} item{col.item_count !== 1 ? 's' : ''}
											{#if col.est_size}· {col.est_size}{:else if col.total_bytes}· {fmtBytes(col.total_bytes)}{/if}
										</div>
										{#if (col.content_types?.length ?? 0) > 0 || col.sorted}
											<div class="col-tags">
												{#each (col.content_types ?? []).slice(0, 3) as t}
													<span class="tag">{t}</span>
												{/each}
												{#if col.sorted}
													<span class="tag tag-sorted">sorted</span>
												{/if}
											</div>
										{/if}
										<span class="private-pill" title="Sealed to you by the owner — not visible to other viewers.">Private</span>
									</button>
								{/each}
							</div>
						{/if}
					</div>
				{/if}

			<!-- File tree -->
			{:else}
				<div class="file-view">
					<!-- devtest v0.12.4 #4: file-view controls — Details/Folders toggle · sort · type filter · search. -->
					<div class="file-toolbar">
						<div class="view-toggle" role="group" aria-label="View mode">
							<button type="button" aria-pressed={viewMode === 'details'} onclick={() => (viewMode = 'details')}>Details</button>
							<button type="button" aria-pressed={viewMode === 'folders'} onclick={() => (viewMode = 'folders')}>Folders</button>
						</div>
						<div class="sort-control">
							<select class="hb-input sort-select" bind:value={sortKey} aria-label="Sort by">
								<option value="name">Name</option>
								<option value="size">Size</option>
								<option value="type">Type</option>
							</select>
							<button type="button" class="sort-dir" onclick={() => (sortDir = sortDir === 'asc' ? 'desc' : 'asc')} title="Sort direction" aria-label="Toggle sort direction">
								{sortDir === 'asc' ? '↑' : '↓'}
							</button>
						</div>
						<div class="hb-input file-search">
							<span class="search-icon">{@html icons.search}</span>
							<input placeholder="Search this collection…" bind:value={fileSearch} aria-label="Search items" />
						</div>
					</div>
					{#if availableTypes.length > 0}
						<div class="type-filter">
							<button type="button" class="type-chip" class:type-chip-active={activeTypes.length === 0} onclick={() => (activeTypes = [])}>All types</button>
							{#each availableTypes as t (t)}
								<button type="button" class="type-chip" class:type-chip-active={activeTypes.includes(t)} onclick={() => toggleType(t)}>{t}</button>
							{/each}
						</div>
					{/if}
					{#if sortedItems.length === 0}
						<div class="empty-state">
							<div class="empty-icon">{@html icons.folder}</div>
							<div class="empty-label">{fileSearch.trim() || activeTypes.length > 0 ? 'No items match your filters' : 'Empty folder'}</div>
						</div>
					{:else if viewMode === 'details'}
						<div class="file-table">
							<div class="file-header">
								<span class="fh-name">Name</span>
								<span class="fh-size">Size</span>
								<span class="fh-type">Type</span>
							</div>
							{#each sortedItems as item (item.name)}
								<button
									class="file-row"
									class:file-folder={item.item_type === 'Folder'}
									class:file-leaf={item.item_type === 'File'}
									onclick={() => { if (item.item_type === 'Folder') enterFolder(item); }}
								>
									<span class="file-icon">
										{@html item.item_type === 'Folder' ? icons.folder : icons.file}
									</span>
									<span class="file-name">{item.name}</span>
									<span class="file-size">{item.size ?? ''}</span>
									<span class="file-type">{item.format ?? ''}</span>
								</button>
							{/each}
						</div>
					{:else}
						<!-- Folders (tile) view — the same metadata as Details, laid out as large icons. -->
						<div class="item-grid">
							{#each sortedItems as item (item.name)}
								<button
									class="item-tile"
									class:file-folder={item.item_type === 'Folder'}
									class:file-leaf={item.item_type === 'File'}
									onclick={() => { if (item.item_type === 'Folder') enterFolder(item); }}
								>
									<div class="item-tile-icon">{@html item.item_type === 'Folder' ? icons.folder : icons.file}</div>
									<div class="item-tile-name">{item.name}</div>
									<div class="item-tile-meta">
										{item.item_type === 'Folder' ? 'Folder' : (item.format ?? 'File')}{#if item.size} · {item.size}{/if}
									</div>
								</button>
							{/each}
						</div>
					{/if}
					{#if paywall}
						<!-- devtest #7: paywall fade — the owner published only a preview of a too-large collection. -->
						<div class="paywall">
							<div class="paywall-fade"></div>
							<div class="paywall-note">
								<span class="paywall-lock">🔒</span>
								<div>
									<div class="paywall-title">{paywall.hidden.toLocaleString()} more item{paywall.hidden !== 1 ? 's' : ''} hidden</div>
									<div class="paywall-sub">Showing {paywall.shown.toLocaleString()} of {paywall.total.toLocaleString()} — this collection is too large to publish in full.</div>
									<!-- M16 W4 + M17 W7.1a: the "get the rest" affordance. The primary "Ask the owner" button
									     becomes the muted "Asked {relative} — waiting for their reply" state after a successful
									     send, with a secondary "Ask again" (60-min cooldown, mirroring announce-cooldown) and an
									     "Open chat" link to where the reply will arrive. Import stays alongside. No Download
									     button (MAS-INV-5): Hoardbook moves no files. -->
									<div class="paywall-actions">
										{#if askState.kind === 'asked'}
											<!-- Asked-state: read from the persisted record, not component-local state. The muted
											     line + cooldown-gated "Ask again" + "Open chat" deep-link (W1) to where the reply lands. -->
											<span class="asked-line">{MANIFEST_ASKED_LINE(askState.relative)}</span>
											<button
												class="btn-ghost btn-sm"
												onclick={handleAskOwner}
												disabled={askingOwner || !askState.cooldownOver}
												title={askState.cooldownOver ? '' : MANIFEST_ASK_AGAIN_COOLDOWN_TIP(askState.cooldownRemaining)}
											>{MANIFEST_ASK_AGAIN_LABEL}</button>
											<a class="btn-ghost btn-sm open-chat-link" href={`/chat?peer=${selectedPeer?.npub ?? ''}`}>{MANIFEST_OPEN_CHAT_LABEL}</a>
										{:else}
											<button class="btn-primary btn-sm" onclick={handleAskOwner} disabled={askingOwner}>Ask the owner for the full list</button>
										{/if}
										<button class="btn-ghost btn-sm" onclick={pickManifestFile} disabled={importingManifest}>Import a manifest file you received</button>
										<button class="btn-ghost btn-sm" onclick={() => (pasteOpen = !pasteOpen)}>or paste it</button>
									</div>
									{#if askError && askState.kind !== 'asked'}
										<!-- Failure is loud (W7.1a): a rejected publish leaves the un-asked state AND shows the
										     muted reason inline. A failed ask must never render as "Asked". -->
										<div class="ask-failed-note">{MANIFEST_ASK_FAILED_LINE(askError)}</div>
									{/if}
									{#if pasteOpen}
										<textarea class="hb-input hb-textarea hb-mono paywall-paste" bind:value={pasteText} placeholder="Paste the .hbmanifest text or its base64 here"></textarea>
										<button class="btn-primary btn-sm" disabled={importingManifest || !pasteText.trim()} onclick={() => handleImportManifest({ pasted: pasteText })}>Import from text</button>
									{/if}
								</div>
							</div>
						</div>
					{/if}
					{#if folderStack.length === 0 && importedManifestNote(selectedCollection)}
						<div class="imported-note"><span>{importedManifestNote(selectedCollection)}</span></div>
					{/if}
					{#if collectionAvailability(selectedCollection)}
						<div class="kofn-note">
							<span>{collectionAvailability(selectedCollection)}</span>
							<FeatureTooltip key="k-of-n-folders" />
						</div>
					{/if}
					<!-- The listing is metadata only — Hoardbook moves no files (H4/INV-4). -->
					<div class="no-download-note">
						<span>Metadata only — Hoardbook moves no files.</span>
						<FeatureTooltip key="no-download" />
					</div>
				</div>
			{/if}
		{/if}
	</div>
</div>

<!-- M22 W3 — the Name moment for Browse's People list. Esc cancels (no write); Enter commits
     (additive — both peers keep every group they were already in and both gain the new one).
     M22 W5 — for a multi-select drop, the two avatars are replaced by a count badge ("N").
     QURATOR-98 — rendered through the shared Modal shell (backdrop, Escape, Tab trap, focus
     restore), replacing the hand-rolled .dg-backdrop/.dg-panel pair that sat at --z-menu, BELOW
     --z-modal. closeOnBackdrop={false}: the field holds a typed group name, and the app's
     typed-content rule keeps a stray outside click from discarding it (Topics/Chat compose).
     minor-6 — the migration dropped the old shell's `aria-label="Name this group"`; Modal only
     wires aria-labelledby when a `title` is passed, and this panel's compact dg-header (padding="0")
     draws its own inline avatars+input row with no room for Modal's visible <h2>. `ariaLabel` gives
     the dialog its accessible name back WITHOUT a visible heading, so the layout stays byte-identical. -->
{#if dragPopoverFor}
	{@const isMulti = Array.isArray(dragPopoverFor)}
	{@const dgSource = !isMulti ? $contacts.find((c) => c.npub === (dragPopoverFor as { source: string; target: string })!.source) : undefined}
	{@const dgTarget = !isMulti ? $contacts.find((c) => c.npub === (dragPopoverFor as { source: string; target: string })!.target) : undefined}
	<Modal open={true} width="300px" padding="0" closeOnBackdrop={false} onclose={closeDragPopover} ariaLabel="Name this group">
		<div class="dg-header">
			<div class="dg-avatars">
				{#if isMulti}
					<span class="dg-count" title={`${(dragPopoverFor as string[]).length} contacts`}>{(dragPopoverFor as string[]).length}</span>
				{:else}
					{#if dgSource}
						{@const dgI = (contactDisplayName(dgSource)[0] ?? '?').toUpperCase()}
						<span class="dg-avatar dg-avatar-a" style={`--dg-hue:${avatarHue(dgI)}`}>{dgI}</span>
					{/if}
					{#if dgTarget}
						{@const dgI = (contactDisplayName(dgTarget)[0] ?? '?').toUpperCase()}
						<span class="dg-avatar dg-avatar-b" style={`--dg-hue:${avatarHue(dgI)}`}>{dgI}</span>
					{/if}
				{/if}
			</div>
			<input class="hb-input dg-input" type="text" placeholder="Name this group" bind:this={dragNameEl} bind:value={dragNameInput} onkeydown={onDragNameKey} />
		</div>
		{#if dragSuggestions.length > 0}
			<div class="dg-suggestions">
				{#each dragSuggestions as s (s)}
					<button type="button" class="dg-chip" onclick={() => (dragNameInput = s)}>{s}</button>
				{/each}
			</div>
		{/if}
		<div class="dg-footer">
			<button type="button" class="btn-ghost btn-xs" onclick={closeDragPopover}>Cancel</button>
			<button type="button" class="btn-primary btn-xs" disabled={dragNameInput.trim().length === 0} onclick={commitDragCreate}>Create</button>
		</div>
	</Modal>
{/if}

<!-- M22 W8 — the ONE group-membership editor (the keyboard route for add/move/ungrouped). Same
     component + same full-set contactUpdateGroups write as Contacts, so the two surfaces cannot
     drift. Always rendered (not {#if}-wrapped) so the component's open/close effects run: seeding the
     draft + focusing in on open, and returning focus to the row on close. -->
<GroupMembershipPopover
	open={groupPopoverFor !== null}
	anchor={groupPopoverAnchor}
	contactName={gmpContactName}
	groups={groups}
	memberships={gmpMemberships}
	ready={groupsLoaded}
	onapply={(names) => groupPopoverFor && applyGroupPopover(groupPopoverFor, names)}
	onclose={() => (groupPopoverFor = null)}
	onnewgroup={() => (createGroupOpen = true)}
	returnFocusTo={() => {
		// Return focus to the row that opened the editor (or the first rendered row if it was
		// filtered away — A9: .focus() on a detached node is a silent no-op).
		// onclose sets groupPopoverFor = null BEFORE this runs, so reading it here returned undefined
		// and focus was stranded on <body> (Codex review 2026-08-11). Use the npub captured at open.
		if (!gmpOpenedFor) return undefined;
		const el = document.getElementById(rowId(ROW_ID_PREFIX, gmpOpenedFor));
		if (el?.isConnected) return el;
		return document.getElementById(rowId(ROW_ID_PREFIX, contactOrder[0])) ?? undefined;
	}}
/>
<CreateGroupDialog open={createGroupOpen} oncreate={handleCreateGroup} oncancel={() => (createGroupOpen = false)} />

<style>
	.browse-shell {
		display: flex;
		height: 100%;
		overflow: hidden;
	}

	/* ── Left panel ──────────────────────────────────────────────── */

	.left-panel {
		width: 216px;
		flex-shrink: 0;
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	.panel-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 14px 14px 10px;
		border-bottom: 1px solid var(--divider);
	}

	.panel-title {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.6px;
		text-transform: uppercase;
		color: var(--fg-dim);
	}

	/* QURATOR-101 — on the .hb-input contract; only the icon-prefix layout (gap, outer inset) is
	   local. The inner input is transparent/borderless so the wrapper reads as one input field. */
	.search-wrap {
		gap: 6px;
		margin: 8px 10px;
		flex-shrink: 0;
	}

	.search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }

	.search-input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 12px;
		color: var(--fg);
		font-family: var(--font-ui);
	}

	.search-input::placeholder { color: var(--fg-dim); }

	.contact-list {
		overflow-y: auto;
		flex: 1;
	}

	/* M21 W5b: People panel grouped by the user's groups (mirrors Contacts' Groups view). */
	.people-section { display: flex; flex-direction: column; }
	.people-section-head {
		display: flex; align-items: center; gap: 5px;
		padding: 8px 12px 4px;
	}
	.people-section-title {
		font-size: 9.5px; font-weight: 700; letter-spacing: 0.6px; text-transform: uppercase;
		color: var(--fg-dim);
	}
	.people-section-count {
		font-size: 9.5px; color: var(--fg-dim); font-feature-settings: 'tnum';
	}
	.people-group-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; display: inline-block; }

	.left-empty {
		padding: 16px;
		font-size: 12px;
		color: var(--fg-dim);
		text-align: center;
	}

	.contact-row {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 5px 4px;
		border-radius: 5px;
		font-size: 12px;
		background: transparent;
		border: none;
		cursor: pointer;
		width: 100%;
		text-align: left;
		transition: background 0.1s;
	}

	.contact-row:hover { background: var(--bg-elev1); }
	.contact-selected { background: var(--bg-elev2) !important; }

	.avatar-wrap {
		position: relative;
		flex-shrink: 0;
	}

	.online-dot {
		position: absolute;
		bottom: -1px;
		right: -1px;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--online);
		border: 1.5px solid var(--bg);
	}

	/* devtest v0.12.1 #3: browse-key lock/unlock badge, overlapping the avatar's top-right corner
	   (mirrors the bottom-right online dot). Shown for both states (🔓 browseable / 🔒 key needed). */
	.access-lock {
		position: absolute;
		top: -6px;
		right: -6px;
		font-size: 13px;
		line-height: 1;
		padding: 1px 2px;
		border-radius: 999px;
		background: var(--bg);
		box-shadow: 0 0 0 1px var(--border);
	}

	.contact-name {
		font-size: 12px;
		font-weight: 500;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.contact-meta {
		font-size: 10px;
		color: var(--fg-dim);
		margin-left: auto;
		flex-shrink: 0;
		font-feature-settings: 'tnum';
	}

	/* ── Right panel ─────────────────────────────────────────────── */

	.right-panel {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

	.empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 10px;
		color: var(--fg-dim);
	}

	.empty-icon {
		opacity: 0.3;
		transform: scale(2.8);
		margin-bottom: 8px;
		display: flex;
	}

	.empty-label { font-size: 12.5px; display: inline-flex; align-items: center; }

	.empty-cta {
		font-size: 12px;
		color: var(--accent);
		text-decoration: none;
		margin-top: 4px;
	}
	.empty-cta:hover { text-decoration: underline; }

	/* Willing-to hints bar */
	.willing-bar {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 6px;
		padding: 8px 16px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}

	.willing-label {
		display: inline-flex;
		align-items: center;
		font-size: 10.5px;
		font-weight: 600;
		letter-spacing: 0.3px;
		text-transform: uppercase;
		color: var(--fg-dim);
	}

	.willing-chip {
		font-size: 11px;
		padding: 2px 8px;
		border-radius: 999px;
		background: var(--accent-soft);
		color: var(--accent);
	}

	/* K-of-N availability note (M13 HANDOVER gap #5) */
	.kofn-note {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 10px 16px;
		border-top: 1px solid var(--divider);
		font-size: 11.5px;
		color: var(--fg-dim);
		flex-shrink: 0;
	}

	/* devtest #7: paywall teaser — a gradient fade over the last rows + a "N more hidden" note. */
	.paywall { position: relative; flex-shrink: 0; }
	.paywall-fade {
		height: 56px;
		margin-top: -56px;
		pointer-events: none;
		background: linear-gradient(to bottom, transparent, var(--bg) 92%);
	}
	.paywall-note {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 16px 16px;
		color: var(--fg-muted);
	}
	.paywall-lock { font-size: 16px; flex-shrink: 0; }
	.paywall-title { font-size: 12.5px; font-weight: 600; color: var(--fg); }
	.paywall-sub { font-size: 11.5px; color: var(--fg-dim); margin-top: 1px; }

	/* M16 W4: the "get the rest" affordances inside the paywall note. */
	.paywall-actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; align-items: center; }
	.paywall-paste { display: block; width: 100%; margin-top: 6px; min-height: 52px; }
	/* M17 W7.1a: the muted asked-state line + the inline failure reason. */
	.asked-line { font-size: 11.5px; color: var(--fg-dim); }
	.ask-failed-note { font-size: 11.5px; color: var(--error); margin-top: 6px; }
	.imported-note {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 16px;
		font-size: 11.5px;
		color: var(--online);
	}

	/* No-download footer note */
	.no-download-note {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 10px 16px;
		margin-top: auto;
		border-top: 1px solid var(--divider);
		font-size: 11.5px;
		color: var(--fg-dim);
		flex-shrink: 0;
	}

	/* Breadcrumb */

	.breadcrumb {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 9px 14px;
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
		flex-wrap: wrap;
		min-height: 38px;
	}

	.bc-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		font-size: 12px;
		font-weight: 500;
		color: var(--fg-muted);
		padding: 2px 5px;
		border-radius: 4px;
		font-family: var(--font-ui);
		transition: background 0.1s, color 0.1s;
	}

	.bc-btn:hover {
		background: var(--bg-elev2);
		color: var(--fg);
	}

	.bc-sep {
		color: var(--fg-dim);
		display: flex;
		align-items: center;
		padding: 0 1px;
	}

	.bc-current {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg);
		padding: 2px 5px;
	}

	/* Collections grid */

	.col-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(158px, 1fr));
		gap: 10px;
		padding: 16px;
		overflow-y: auto;
		align-content: start;
	}

	/* QURATOR-92 — the Private section under the public grid. The pill matches CollectionRow's
	   Home-page Private badge (accent text + 30%-mix accent border), so the affordance reads the
	   same everywhere a collection's visibility is shown. */
	.private-collections { display: flex; flex-direction: column; }
	.private-collections .col-grid { border-top: 1px solid var(--divider); }
	.private-collections-label {
		display: flex; align-items: center; gap: 6px;
		padding: 10px 16px 0;
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
	}
	.private-pill {
		font-size: 9.5px; padding: 1px 6px; border-radius: 999px; letter-spacing: 0.5px;
		border: 1px solid color-mix(in oklch, var(--accent) 30%, transparent);
		background: color-mix(in oklch, var(--accent) 10%, transparent); color: var(--accent);
		width: fit-content;
	}

	.col-card {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 12px;
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 8px;
		cursor: pointer;
		text-align: left;
		transition: background 0.1s, border-color 0.1s;
	}

	.col-card:hover {
		background: var(--bg-elev2);
	}

	.col-card-icon {
		color: var(--accent);
		margin-bottom: 4px;
		display: flex;
	}

	.col-card-name {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--fg);
		word-break: break-word;
	}

	.col-card-desc {
		font-size: 11px;
		color: var(--fg-muted);
		overflow: hidden;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}

	.col-card-meta {
		font-size: 10.5px;
		color: var(--fg-dim);
		margin-top: 2px;
	}

	.col-tags {
		display: flex;
		flex-wrap: wrap;
		gap: 3px;
		margin-top: 4px;
	}

	.tag {
		font-size: 9.5px;
		padding: 1px 5px;
		border-radius: 999px;
		background: var(--bg-elev3);
		color: var(--fg-muted);
		/* W6: the plain chip's border is gone (fill + spacing separate it). The transparent base
		   stays so `.tag-sorted` can still draw its accent ring — Sorted is a state signal, not
		   chrome, and the same pattern keeps `.filter-tag-active` / `.type-chip-active` working. */
		border: 1px solid transparent;
	}

	.tag-sorted {
		background: var(--accent-soft);
		color: var(--accent);
		border-color: color-mix(in oklch, var(--accent) 30%, transparent);
	}

	/* File view */

	.file-view {
		flex: 1;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
	}

	/* devtest v0.12.4 #4: file-view toolbar (Details/Folders · sort · search) + type-filter chips. */
	.file-toolbar {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		padding: 10px 14px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}
	.view-toggle {
		display: flex;
		border: 1px solid var(--border);
		border-radius: 7px;
		overflow: hidden;
		flex-shrink: 0;
	}
	.view-toggle button {
		padding: 5px 11px;
		font-size: 12px;
		font-weight: 600;
		background: transparent;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		font-family: var(--font-ui);
	}
	.view-toggle button[aria-pressed='true'] { background: var(--accent-soft); color: var(--accent); }

	.sort-control { display: flex; align-items: center; gap: 4px; flex-shrink: 0; }
	/* QURATOR-101 — on the .hb-input contract; only cursor is local. */
	.sort-select { cursor: pointer; }
	.sort-dir {
		width: 28px; height: 28px; flex-shrink: 0;
		display: flex; align-items: center; justify-content: center;
		background: var(--bg-input); border: 1px solid var(--border); border-radius: 6px;
		color: var(--fg-muted); cursor: pointer; font-size: 13px; line-height: 1;
	}
	.sort-dir:hover { color: var(--fg); border-color: var(--border-strong); }

	/* QURATOR-101 — on the .hb-input contract; only the flex-sizing layout is local. */
	.file-search {
		gap: 6px;
		flex: 1; min-width: 140px; max-width: 280px;
	}
	.file-search .search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }
	.file-search input {
		flex: 1; min-width: 0; background: transparent; border: none; outline: none;
		font-size: 12px; color: var(--fg); font-family: var(--font-ui);
	}
	.file-search input::placeholder { color: var(--fg-dim); }

	.type-filter {
		display: flex; flex-wrap: wrap; gap: 6px;
		padding: 8px 14px; border-bottom: 1px solid var(--divider); flex-shrink: 0;
	}
	.type-chip {
		padding: 2px 10px; font-size: 11px; font-weight: 500;
		border: 1px solid transparent; border-radius: 999px;
		background: transparent; color: var(--fg-muted); cursor: pointer;
		font-family: var(--font-ui);
	}
	.type-chip:hover { border-color: var(--accent); color: var(--accent); }
	.type-chip-active { background: var(--accent-soft); border-color: var(--accent); color: var(--accent); }

	/* Folders (tile) view — the same metadata as Details, laid out as large icons. */
	.item-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
		gap: 10px;
		padding: 14px;
		align-content: start;
	}
	.item-tile {
		display: flex; flex-direction: column; align-items: center; gap: 4px;
		padding: 14px 10px;
		background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 8px;
		text-align: center; color: inherit; font-family: inherit;
		cursor: default; min-width: 0;
	}
	.item-tile.file-folder { cursor: pointer; }
	.item-tile:hover { background: var(--bg-elev2); }
	.item-tile-icon { color: var(--fg-muted); display: flex; transform: scale(1.4); margin: 4px 0 8px; }
	.item-tile.file-folder .item-tile-icon { color: var(--accent); }
	.item-tile-name {
		font-size: 12px; color: var(--fg); font-weight: 500;
		overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%;
	}
	.item-tile-meta { font-size: 10px; color: var(--fg-dim); }

	.file-table {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.file-header {
		display: grid;
		grid-template-columns: 1fr 80px 90px 28px;
		padding: 6px 14px 6px 40px;
		border-bottom: 1px solid var(--border);
		position: sticky;
		top: 0;
		background: var(--bg);
		z-index: 1;
		flex-shrink: 0;
	}

	.fh-name, .fh-size, .fh-type {
		font-size: 10.5px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--fg-dim);
	}

	.fh-size, .fh-type { text-align: right; }

	.file-row {
		display: grid;
		grid-template-columns: 20px 1fr 80px 90px 28px;
		align-items: center;
		padding: 5px 14px;
		background: transparent;
		border: none;
		width: 100%;
		text-align: left;
		gap: 0;
		transition: background 0.1s;
		column-gap: 6px;
	}

	.file-row:hover { background: var(--bg-elev1); }

	.file-folder { cursor: pointer; }
	.file-leaf { cursor: default; }

	.file-icon {
		display: flex;
		align-items: center;
		color: var(--fg-muted);
		grid-column: 1;
	}

	.file-folder .file-icon { color: var(--accent); }

	.file-name {
		font-size: 12px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		grid-column: 2;
	}

	.file-size {
		font-size: 11px;
		color: var(--fg-dim);
		text-align: right;
		font-family: var(--font-mono);
		grid-column: 3;
	}

	.file-type {
		font-size: 11px;
		color: var(--fg-dim);
		text-align: right;
		grid-column: 4;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* M22 W3 — drag-to-group gesture on the People list.
	   M22 W5 — .peer-selected marks the multi-drag selection (distinct from .contact-selected
	   which means "shown in the right panel"). Soft accent fill + left border, persistent. */
	.contact-row.peer-selected {
		background: color-mix(in oklch, var(--accent) 10%, var(--bg-elev1));
		box-shadow: inset 0 0 0 2px var(--accent);
	}
	.contact-row.drag-source { opacity: 0.35; }
	.contact-row.drag-target {
		border-color: var(--accent) !important;
		background: color-mix(in oklch, var(--accent) 8%, var(--bg-elev1));
	}
	.drag-outcome-browse {
		font-size: 10px; font-weight: 600; color: var(--accent); margin-left: auto; flex-shrink: 0;
	}

	/* M22 W3 — naming popover content (QURATOR-98: the shell is the shared Modal; only the
	   namer-specific inner styles stay). The input's contract is the global .hb-input —
	   .dg-input contributes layout only, matching Contacts' twin. */
	.dg-header { display: flex; align-items: center; gap: 10px; padding: 6px 8px; }
	.dg-avatars { display: flex; position: relative; width: 44px; height: 30px; flex-shrink: 0; }
	.dg-avatar {
		width: 30px; height: 30px; border-radius: 50%;
		display: flex; align-items: center; justify-content: center;
		font-size: 12px; font-weight: 600; color: white;
		background: oklch(0.55 0.15 var(--dg-hue));
		border: 2px solid var(--bg-elev2);
	}
	.dg-avatar-b { margin-left: -14px; }
	/* M22 W5 — count badge replaces the two avatars for a multi-select drop. */
	.dg-count {
		width: 44px; height: 30px; border-radius: 15px;
		display: flex; align-items: center; justify-content: center;
		font-size: 13px; font-weight: 700; color: white;
		background: var(--accent);
		flex-shrink: 0;
	}
	.dg-input { flex: 1; padding: 4px 8px; min-width: 0; }
	.dg-suggestions { display: flex; flex-wrap: wrap; gap: 5px; padding: 0 8px 4px; }
	.dg-chip {
		font-size: 11px; padding: 2px 8px; border-radius: 999px;
		background: var(--bg-elev3); border: 1px solid transparent; color: var(--fg-muted);
		cursor: pointer; font-family: var(--font-ui);
	}
	.dg-chip:hover { border-color: var(--accent); color: var(--accent); }
	.dg-footer { display: flex; justify-content: flex-end; gap: 6px; padding: 6px 8px 2px; border-top: 1px solid var(--divider); margin-top: 4px; }

	/* M22 W4 — drop-onto-group affordances on the People section heads. Mirrors Contacts. */
	.people-section-head.group-drop-active {
		background: color-mix(in oklch, var(--accent) 8%, transparent);
	}
	.people-section-head.group-drop-active .people-section-title { color: var(--accent); }
	.people-section-head.group-drop-refused { opacity: 0.5; }
	.drop-hint-browse {
		font-size: 9px; font-weight: 600; color: var(--accent); text-transform: none; letter-spacing: 0;
	}
	.people-section-head.group-drop-refused .drop-hint-browse { color: var(--fg-dim); }

	/* M22 W7 — screen-reader-only live region (the refuse-state mirror). */
	.sr-only {
		position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
		overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0;
	}

	/* M22 W7 — respect prefers-reduced-motion: disable the transition on the compact People rows. */
	@media (prefers-reduced-motion: reduce) {
		.contact-row { transition: none; }
	}
</style>
