<script lang="ts">
	import { follow, refreshContact, unfollowContact, setContactTags, groupsGet, groupsCreate, groupsDelete, groupsAssign, groupsUnassign, groupsCreateWithMembers, contactUpdateGroups, browsePrivateCollections, onlineCount, relayStatus, getContacts, privateAudienceList, privateAudienceSet, type OnlineCount, type RelayHealth } from '$lib/api.js';
	import { contacts, toast, toastWithAction } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import CollectionPanel from '$lib/components/CollectionPanel.svelte';
	import OverflowMenu from '$lib/components/OverflowMenu.svelte';
	// M22 W8 — ONE shared group-membership editor (extracted from the W5b inline popover) used by
	// BOTH Contacts and Browse. The draft, checkboxes, Apply/contactUpdateGroups, "+ New group…" and
	// focus-return all live in the component; this page supplies the anchor + callbacks.
	import GroupMembershipPopover from '$lib/components/GroupMembershipPopover.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';
	import CreateGroupDialog from '$lib/components/CreateGroupDialog.svelte';
	import AddContactDialog from '$lib/components/AddContactDialog.svelte';
	import AddContactPanel from '$lib/components/AddContactPanel.svelte';
	import AZRail from '$lib/components/AZRail.svelte';
	import type { CachedPeer, Collection, Group } from '$lib/types.js';
	import { contactDisplayName, shortNpub } from '$lib/contact-display.js';
	import { NOT_DRM_NOTE, receivesPrivate } from '$lib/private-collections-view.js';
	import { peerAccessBadge, summarizeCollectionsSize } from '$lib/browse-view.js';
	import { onlineChipView } from '$lib/online-chip.js';
	// M21 W4 — the petnameFor collision warning is the card-face security control (the coloured
	// fingerprint is only ~36 bits and grindable; this badge is what actually flags impersonation).
	import { petnameFor } from '$lib/identity-display.js';
	// M21 W4 — fixed word→hue table for the coloured fingerprint (rendering-only; Rust picks the word).
	import { fingerprintWordColor } from '$lib/fingerprint-colors.js';
	// M23 W6 — pure overflow predicate for the bio `more ⌄` control (the DOM read lives in the
	// `bioMeasure` action below; this seam is what the unit test pins).
	import { bioOverflows } from '$lib/bio-overflow.js';
	// M17 W5 — presence honesty: "checked {t}" (our cache) vs "Last seen {t}" (their beacon).
	import { PRESENCE_TICK_MS, checkedLabel, freshIndex, newestSeen, presenceView, type PresenceView } from '$lib/presence-view.js';
	import { relayWhyHint } from '$lib/relay-health.js';
	import { ONLINE_POLL_VISIBLE_MS } from '$lib/poll-lifecycle.js';
	import { ALPHABET, groupByLetter, groupByGroups, onlineBucket, matchesQuery, presentSectionKeys } from '$lib/contacts-view.js';
	// M22 W3 — drag-to-group gesture primitives (shared with Browse). Create is ALWAYS ADDITIVE
	// (Reading B): both peers keep every group they were already in and both gain the new one.
	// M22 W4 — drop onto an existing group: plain drop MOVES, Shift-drop ADDS (owner ruling
	// 2026-08-09 inverted the earlier inferred rule). Refused before release; Ungrouped clears all.
	// M22 W5 — multi-select drag: Shift-click extends a contiguous run, Cmd/Ctrl toggles one row,
	// plain click selects one. Dragging any selected row carries the WHOLE selection; dragging an
	// unselected row carries just that row. Ghost shows a count badge. Refuse over selection: a
	// target ALL selected already belong to refuses; MIXED allowed.
	import { writeDragPayload, readDragPayload, writeDragPayloadMulti, readDragPayloadMulti, isValidDropTarget, isSelfDrop, groupSuggestions, groupSuggestionsMulti, commitCreateGroup, commitCreateGroupMulti, computeDropOutcome, commitDropOnGroup, computeDropOutcomeMulti, commitDropOnGroupMulti, applyClickToSelection, applyKeyToSelection, shouldHandleArrowKey, computeDropInverse, computeDropInverseMulti, computeCreateInverse, commitInverse, commitInverseMulti, rovingTabindexForIdx, isTypingTargetShape, rowId, UNGROUPED_TARGET, type DropOutcome, type DropOutcomeMulti } from '$lib/drag-group.js';
	import { onMount, onDestroy, tick } from 'svelte';
	import { goto } from '$app/navigation';

	// ── "🟢 N hoarders online" chip (M9) — relay-derived, no telemetry. **Always shown** (the Settings
	//    hide-toggle was removed); lives here on Contacts. Best-effort + cached on the backend; polled on
	//    a slow tick (L4-budgeted); shows "–" while the count is unknown (m4).
	let onlineData: OnlineCount | null = $state(null);
	let relayHealth: RelayHealth[] = $state([]);
	let chip = $derived(onlineChipView(onlineData, true, relayHealth));
	// M12 W1 Decision D: when the chip can't show a number, say *why* (which relays are unreachable).
	let whyHint = $derived(chip.unknown ? relayWhyHint(relayHealth) : '');
	let onlinePollTimer: ReturnType<typeof setInterval> | undefined;
	async function refreshOnline() {
		// Decision B: don't poll the relays while the window is hidden (tray/minimized).
		if (document.hidden) return;
		try { onlineData = await onlineCount(); } catch { /* keep last value; chip shows "–" */ }
		// Drive the "why" hint only when the count is unknown (cheap, status-only read).
		try { relayHealth = await relayStatus(); } catch { /* leave last health */ }
	}
	// W5 review: the age needs its OWN reactive clock. Reading Date.now() inside a template helper
	// makes the render depend on the 60s poll assigning `onlineData` — so a rejected poll, or a tab
	// hidden past the window (the poll returns early while hidden), leaves a peer pinned "Online"
	// and the age frozen. That is the bug W5 exists to kill, wearing a different hat. This tick is
	// local-only: no relay traffic, and it keeps running while hidden so the first paint after a
	// return is already correct.
	let nowMs = $state(Date.now());
	const clockTimer = setInterval(() => { nowMs = Date.now(); }, PRESENCE_TICK_MS);
	onDestroy(() => {
		if (onlinePollTimer) clearInterval(onlinePollTimer);
		clearInterval(clockTimer);
	});


	// Groups state
	let groups: Group[] = $state([]);
	// Has the FIRST groupsGet() resolved? `contactGroups()` filters this array, so an editor opened
	// before it loads would seed an EMPTY draft and Apply would wipe the contact's memberships
	// (Codex review 2026-08-11). Passed to GroupMembershipPopover as `ready`.
	let groupsLoaded = $state(false);

	async function loadGroups() {
		try { groups = await groupsGet(); groupsLoaded = true; } catch { /* non-fatal */ }
	}

	// M10: Private collections trusted peers have sealed to me, keyed by author npub. A non-trusted
	// viewer simply has no entry — there is no locked-teaser hint.
	let privateByAuthor: Record<string, Collection[]> = $state({});

	async function loadPrivate() {
		try {
			const groups = await browsePrivateCollections();
			const map: Record<string, Collection[]> = {};
			for (const g of groups) map[g.npub] = g.collections;
			privateByAuthor = map;
		} catch { /* non-fatal — relays may be unreachable */ }
	}

	// M21 W5: the Private-collection audience — an explicit per-contact list, decoupled from groups
	// (owner ruling 2026-08-04). Each npub here receives a sealed copy of every Private collection.
	let privateAudience: string[] = $state([]);

	async function loadPrivateAudience() {
		try { privateAudience = await privateAudienceList(); } catch { /* non-fatal */ }
	}

	// Toggle whether a contact receives Private collections (M21 W5). Idempotent on the backend;
	// a removed recipient stops receiving on the *next* republish only (not retroactively — the
	// honest "not DRM" caveat, surfaced via NOT_DRM_NOTE).
	async function toggleReceivesPrivate(npub: string) {
		const next = !receivesPrivate(npub, privateAudience);
		try {
			await privateAudienceSet(npub, next);
			await loadPrivateAudience();
		} catch (e) { toast(String(e), 'error'); }
	}

	// Delete a group (devtest #2) — the backend (groups_delete) moves members to Ungrouped.
	async function handleDeleteGroup(g: Group) {
		try {
			await groupsDelete(g.name);
			await loadGroups();
			toast(`Group "${g.name}" deleted`);
		} catch (e) { toast(String(e), 'error'); }
	}

	// "+ New group" (M13 W5) — renders regardless of how many groups already exist, so a group is
	// always creatable, not just from an existing contact's group picker.
	let createGroupOpen = $state(false);

	async function handleCreateGroup(detail: { name: string; color: string }) {
		const { name, color } = detail;
		try {
			await groupsCreate(name, color);
			await loadGroups();
			createGroupOpen = false;
			toast(`Group "${name}" created`);
		} catch (e) { toast(String(e), 'error'); }
	}

	// M22 W3 — drag-to-group gesture ("drag one user onto another creates an ad hoc group").
	// Three moments: Lift (source dims in place), Aim (target lights + states outcome in words),
	// Name (popover with focused text field + up to 3 suggestion chips). Esc cancels the whole
	// gesture — no group created, nothing written. Create is ALWAYS ADDITIVE: both peers keep
	// every group they were already in and both gain the new one.
	//
	// M22 W5 — multi-select: the selection model is the standard one. Plain click selects one and
	// clears the rest; Shift-click extends a contiguous run from the anchor; Cmd/Ctrl toggles one.
	// Dragging any selected row carries the WHOLE selection; dragging an unselected row carries
	// just that row and clears selection. Ghost shows a count badge. Selection clears after a
	// successful drop and on Esc.
	let selectedNpubs = $state<string[]>([]);             // the multi-selection (empty when idle)
	let selectionAnchor = $state<string | null>(null);    // last plain-clicked row (for Shift range)
	let selectedNpubSet = $derived(new Set(selectedNpubs));
	// M22 W7 — the keyboard-focused row (roving tabindex). Arrow keys move it; with Shift they
	// extend the selection. null until the user first arrows into the list; the first arrow picks
	// the nearest end so the very first press is never a no-op.
	let focusedNpub = $state<string | null>(null);
	// The index of the focused row in contactOrder (or null when focusedNpub is null/filtered-out).
	// Kept in sync so rovingTabindexForIdx is a pure function of (focusedIdx, renderIdx).
	let focusedIdx = $state<number | null>(null);
	// M22 W7 A2 — the list container, so we can check whether focus is within it (A7) and move real
	// DOM focus to a row after an arrow-key move.
	let listContainer: HTMLElement | undefined = $state();
	const ROW_ID_PREFIX = 'contact-row';

	// The three standard click modifiers applied to the multi-selection. onmousedown (not onclick)
	// so the selection is updated BEFORE dragstart fires — dragstart reads the current selection to
	// decide whether to carry the whole set or just the dragged row.
	function onContactMouseDown(e: MouseEvent, npub: string) {
		// Ignore clicks on an inner control (chevron, Browse, Message, ⋯) so they keep their own
		// action instead of also toggling the selection.
		if ((e.target as HTMLElement).closest('button, a')) return;
		const r = applyClickToSelection(selectedNpubs, selectionAnchor, contactOrder, npub, e.shiftKey, e.metaKey || e.ctrlKey);
		selectedNpubs = r.selection;
		selectionAnchor = r.anchor;
		// M22 W7 — a mouse click also moves the roving-tabindex focus so a subsequent arrow key
		// continues from the clicked row (matches every file manager).
		focusedNpub = npub;
		focusedIdx = contactOrder.indexOf(npub);
	}

	// Esc clears the selection (and the W3 naming popover handles its own Esc separately).
	// M22 W7 — ArrowUp/ArrowDown move the roving-tabindex focus; with Shift they extend the
	// selection. `G` opens the SAME namer the drag path uses (dragPopoverFor = [...selected]),
	// so W2 suggestions and W6 create-undo come along for free — there is no parallel create path.
	// The `G` handler is guarded against eating typing: it ignores keys from input/textarea/select/
	// contenteditable targets, the namer's own field, the W5b popover, and any modifier held.
	function isTypingTarget(e: KeyboardEvent): boolean {
		// A8: a window keydown retargets to the shadow host for events from inside a Shadow DOM;
		// composedPath()[0] is the actual inner element. Combined with the pure isTypingTargetShape
		// so the guard logic is unit-testable without a DOM.
		const t = (e.composedPath()[0] ?? e.target) as HTMLElement | null;
		if (!(t instanceof HTMLElement)) return false;
		return isTypingTargetShape(t.tagName, t.isContentEditable);
	}

	// M22 W7 A2 — when a row receives focus (by Tab or click), record it so Tab-then-Arrow does
	// not restart from the nearest end.
	function onRowFocus(npub: string) {
		if (focusedNpub !== npub) {
			focusedNpub = npub;
			focusedIdx = contactOrder.indexOf(npub);
		}
	}

	// M22 W7 A7 — true when focus is within the list container (so arrow keys should be handled
	// and preventDefault'd) OR a row is explicitly focused.
	function listHasFocus(): boolean {
		if (focusedNpub !== null) return true;
		if (listContainer && document.activeElement && listContainer.contains(document.activeElement)) return true;
		return false;
	}

	async function moveFocusToRow(npub: string) {
		// A2: move real DOM focus to the newly-focused row after the DOM has updated.
		await tick();
		document.getElementById(rowId(ROW_ID_PREFIX, npub))?.focus();
	}

	function onWindowKeyDown(e: KeyboardEvent) {
		// The namer's own name field handles its own Enter/Esc; don't compete with it.
		if (dragPopoverFor) return;
		// M22 W8 — when the SHARED group-membership editor is open, it owns the keyboard (its
		// checkboxes take arrows/tab natively; OverflowMenu owns Escape). Don't move the list
		// selection underneath an open editor.
		if (groupPopoverFor) return;
		// ArrowUp/ArrowDown move focus / extend selection. A7: only handle/preventDefault when the
		// list actually has focus, so arrows still scroll the page when the user is elsewhere.
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
		// G opens the namer for the current selection — the keyboard equivalent of the W3/W5
		// create gesture. Guarded so it never fires while typing or with a modifier held.
		if ((e.key === 'g' || e.key === 'G') && !isTypingTarget(e) && !e.ctrlKey && !e.metaKey && !e.altKey && !groupPopoverFor && selectedNpubs.length >= 2) {
			e.preventDefault();
			const peers = selectedNpubs
				.map((n) => $contacts.find((c) => c.npub === n))
				.filter((p): p is CachedPeer => !!p);
			if (peers.length < 2) return;
			// M22 W7 — remember the focused row so the namer can return focus on close (MUST be
			// before dragPopoverFor is set, so the success path can restore it).
			dragPopoverReturnFocus = document.activeElement as HTMLElement | undefined;
			dragSuggestions = groupSuggestionsMulti(peers);
			dragNameInput = '';
			dragPopoverFor = [...selectedNpubs];
			focusNamer();
		}
	}

	let dragSourceNpub = $state<string | null>(null);     // the lifted row, or null when idle
	let dragOverNpub = $state<string | null>(null);       // the row currently under the cursor
	// M22 W5: how many contacts are being carried in the current drag (0 for a W3 single-pair drag,
	// N-1 for a multi-select drag of N). Used for the Aim-moment text ("group N contacts").
	let dragCount = $state(0);
	// M22 W6: prior group membership captured at drag START (not drop) so a slow write between
	// start and drop can't race the inverse. Keyed by npub; emptied on dragend.
	let priorGroupsByNpub = $state<Map<string, string[]>>(new Map());
	let dragPopoverFor = $state<string[] | { source: string; target: string } | null>(null);
	let dragPopoverAnchor: HTMLElement | undefined = $state();
	let dragNameInput = $state('');
	let dragSuggestions = $state<string[]>([]);
	// M22 W7 A3 — the namer's input element, bound so we can focus it when the namer opens.
	let dragNameEl: HTMLInputElement | undefined = $state();
	// M22 W7 — the row that had focus before the namer opened, so focus returns to it on close
	// (the keyboard route must not strand the user at the top of the page after naming a group).
	let dragPopoverReturnFocus: HTMLElement | undefined = $state();

	function onDragStart(e: DragEvent, npub: string) {
		if (!e.dataTransfer) return;
		// M22 W5: if the dragged row is part of the current multi-selection, carry the WHOLE
		// selection. If it is NOT selected, this drag carries just this row and clears the
		// selection first (matches every file manager the user has used).
		let carried: string[];
		if (selectedNpubSet.has(npub) && selectedNpubs.length > 1) {
			writeDragPayloadMulti(e.dataTransfer, selectedNpubs);
			dragCount = selectedNpubs.length; // N carried; the target makes N+1 in the new group
			carried = [...selectedNpubs];
		} else {
			// Dragging an unselected row: clear the selection, carry just this row.
			selectedNpubs = [npub];
			selectionAnchor = npub;
			writeDragPayload(e.dataTransfer, npub);
			dragCount = 0; // W3 single-pair drag — the target makes 2
			carried = [npub];
		}
		dragSourceNpub = npub;
		// M22 W6: capture prior membership at drag START (not drop) so the move/ungrouped inverse
		// restores the exact prior state. A slow write between start and drop cannot race this.
		const m = new Map<string, string[]>();
		for (const n of carried) m.set(n, contactGroups(n));
		priorGroupsByNpub = m;
	}

	function onDragOver(e: DragEvent, npub: string) {
		if (!dragSourceNpub) return;
		e.preventDefault(); // allow drop
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
		dragOverNpub = npub;
	}

	function onDragLeave(npub: string) {
		if (dragOverNpub === npub) dragOverNpub = null;
	}

	function onDragEnd() {
		// Fires on the source after a drop OR a cancel (Esc-away). Clears the lift visuals. The
		// naming popover (if opened) stays open until Enter or Esc closes IT — the drag has ended
		// but the create gesture continues at the naming step.
		dragSourceNpub = null;
		dragOverNpub = null;
		dragCount = 0;
		// M22 W4: also clear the group-heading drop affordance state so a cancelled drag doesn't
		// leave a section heading lit.
		dropOverTarget = null;
		dropOutcome = null;
		dropOutcomeMulti = null;
		priorGroupsByNpub = new Map();
	}

	function onDrop(e: DragEvent, targetNpub: string) {
		e.preventDefault();
		// M22 W5: read the multi payload first. If present, this is a multi-select drag — open the
		// naming popover with ALL selected peers (the ghost shows a count badge, not two avatars).
		const multiNpubs = readDragPayloadMulti(e.dataTransfer);
		if (multiNpubs && multiNpubs.length > 1) {
			// Drop onto a row that is itself part of the selection is a no-op (self-drop).
			if (multiNpubs.includes(targetNpub)) {
				dragSourceNpub = null;
				dragOverNpub = null;
				return;
			}
			// Build the combined peer list (selection + target) for suggestions.
			const peers = [...multiNpubs, targetNpub]
				.map((n) => $contacts.find((c) => c.npub === n))
				.filter((p): p is CachedPeer => !!p);
			if (peers.length < 2) return;
			dragSuggestions = groupSuggestionsMulti(peers);
			dragNameInput = '';
			dragPopoverAnchor = e.currentTarget as HTMLElement;
			dragPopoverFor = [...multiNpubs, targetNpub];
			focusNamer();
			dragOverNpub = null;
			return;
		}
		// W3 single-peer path (unchanged).
		const sourceNpub = readDragPayload(e.dataTransfer);
		// Self-drop is a no-op, not a one-member group.
		if (!sourceNpub || isSelfDrop(sourceNpub, targetNpub)) {
			dragSourceNpub = null;
			dragOverNpub = null;
			return;
		}
		if (!isValidDropTarget(sourceNpub, targetNpub)) return;
		// Open the naming popover anchored at the drop point. The name field is NOT pre-filled.
		const source = $contacts.find((c) => c.npub === sourceNpub);
		const target = $contacts.find((c) => c.npub === targetNpub);
		if (!source || !target) return;
		dragSuggestions = groupSuggestions(source, target);
		dragNameInput = '';
		dragPopoverAnchor = e.currentTarget as HTMLElement;
		dragPopoverFor = { source: sourceNpub, target: targetNpub };
		focusNamer();
		// Clear the drag-over highlight; the source dims until the popover closes.
		dragOverNpub = null;
	}

	function pickSuggestion(name: string) {
		dragNameInput = name;
	}

	// M22 W7 A3 — focus the namer's field once it has rendered. Without this the popover opens and
	// typing goes nowhere: the window handler ignores keys while the namer is open, so the gesture
	// dead-ends. Awaits tick() because the input does not exist until the {#if} block renders.
	async function focusNamer() {
		await tick();
		dragNameEl?.focus();
	}

	function closeDragPopover() {
		dragPopoverFor = null;
		dragNameInput = '';
		dragSuggestions = [];
		// M22 W7 A9 — return focus to the row that opened the namer, but only if it is still
		// connected to the DOM (a detached element's .focus() is a silent no-op). Fall back to the
		// first rendered row so focus is never stranded.
		if (dragPopoverReturnFocus?.isConnected) {
			dragPopoverReturnFocus.focus();
		} else if (contactOrder.length > 0) {
			document.getElementById(rowId(ROW_ID_PREFIX, contactOrder[0]))?.focus();
		}
		dragPopoverReturnFocus = undefined;
	}

	// Esc on the naming field cancels the whole gesture (owner ruling 2026-08-09) — no group
	// created, nothing written. Enter commits via commitDragCreate.
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
		// Close the popover first so a double-Enter can't fire two creates. A4: route through
		// closeDragPopover so focus is restored on the SUCCESS path too (not just cancel).
		closeDragPopover();
		try {
			if (Array.isArray(npubs)) {
				// M22 W5 multi-select create: ONE call with all N npubs.
				await commitCreateGroupMulti({ groupsCreateWithMembers }, name, npubs, groups);
			} else {
				// W3 single-pair create.
				await commitCreateGroup(
					{ groupsCreateWithMembers },
					name,
					npubs.source,
					npubs.target,
					groups,
				);
			}
			await loadGroups(); // refresh so the new chip appears on both cards without a reload
			// M22 W6: create's inverse is delete (the group was brand new, so safe).
			toastWithAction(`Group "${name.trim()}" created`, {
				label: 'Undo',
				run: () => {
					commitInverse({ groupsDelete, groupsUnassign, contactUpdateGroups }, computeCreateInverse(name.trim()))
						.then(() => loadGroups())
						.catch((e) => toast(String(e), 'error'));
				},
			});
			// M22 W5: clear the selection after a successful create.
			selectedNpubs = [];
			selectionAnchor = null;
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// ── M22 W4 — drop onto an existing group (heading, chip, or Ungrouped) ───────────────
	// Owner ruling 2026-08-09 (inverted from the inferred rule): a plain drop MOVES the contact
	// into the target; holding Shift ADDS (preserves existing memberships). The refuse state
	// ("already in Film") is computed on dragover so the cursor shows it is not allowed BEFORE the
	// drop. Ungrouped writes immediately and irreversibly: NO confirm, NO undo (owner: "This isn't
	// a word doc."). Nothing on this path reads or writes the private audience.
	let dropOverTarget: string | null = $state(null);     // group name or UNGROUPED_TARGET under the cursor
	let dropOutcome: DropOutcome | null = $state(null);   // computed on dragover, shown as the affordance
	// M22 W5: the multi-select affordance (parallel to dropOutcome for the single-source case).
	let dropOutcomeMulti: DropOutcomeMulti | null = $state(null);

	// M22 W6: the toast names the affected contact, not just the group. Delegates to the canonical
	// contactDisplayName (petname → display_name → shortNpub) rather than re-deriving the fallback,
	// so a toast can never name a contact differently from the row the user just dragged.
	function dropName(npub: string): string {
		const c = $contacts.find((p) => p.npub === npub);
		return c ? contactDisplayName(c) : shortNpub(npub);
	}

	// M22 W6: show a toast with an Undo button. Undo executes the inverses (local-only — no relay
	// traffic, nothing to retract remotely) then refreshes the group list so chips update.
	function registerUndo(label: string, inverses: import('$lib/drag-group.js').DropInverse[]) {
		toastWithAction(label, {
			label: 'Undo',
			run: () => {
				commitInverseMulti({ groupsDelete, groupsUnassign, contactUpdateGroups }, inverses)
					.then(() => loadGroups())
					.catch((e) => toast(String(e), 'error'));
			},
		});
	}

	function onGroupDragOver(e: DragEvent, targetName: string) {
		// M22 W5: read the multi payload first; fall back to single-npub.
		const multiNpubs = readDragPayloadMulti(e.dataTransfer);
		if (multiNpubs && multiNpubs.length > 1) {
			e.preventDefault(); // allow drop
			const groupsByNpub = new Map(multiNpubs.map((n) => [n, contactGroups(n)]));
			const outcome = computeDropOutcomeMulti(multiNpubs, targetName, groupsByNpub, e.shiftKey);
			dropOverTarget = targetName;
			dropOutcomeMulti = outcome;
			if (e.dataTransfer) {
				e.dataTransfer.dropEffect = (outcome.kind === 'refused' || outcome.kind === 'noop') ? 'none' : (e.shiftKey ? 'copy' : 'move');
			}
			return;
		}
		const sourceNpub = readDragPayload(e.dataTransfer);
		// No payload = wrong drag type (e.g. a file drag) — don't claim the drop.
		if (!sourceNpub) return;
		e.preventDefault(); // allow drop
		const sourceGroups = contactGroups(sourceNpub);
		const outcome = computeDropOutcome(sourceNpub, targetName, sourceGroups, e.shiftKey);
		dropOverTarget = targetName;
		dropOutcome = outcome;
		// The cursor must show the refuse state on dragover, not on drop.
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
		// M22 W5: multi-select drop onto a group — compute over the whole selection.
		const multiNpubs = readDragPayloadMulti(e.dataTransfer);
		if (multiNpubs && multiNpubs.length > 1) {
			e.preventDefault();
			const groupsByNpub = new Map(multiNpubs.map((n) => [n, contactGroups(n)]));
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
				await loadGroups();
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
				// Clear the selection after a successful drop.
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
		const outcome = dropOutcome ?? computeDropOutcome(sourceNpub, targetName, contactGroups(sourceNpub), e.shiftKey);
		// Refused / noop: no write, no toast. The affordance already said why.
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
			await loadGroups(); // refresh so the chip appears/disappears without a reload
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

	// Stale: last_fetched more than 7 days ago.
	function isStale(peer: CachedPeer): boolean {
		if (!peer.last_fetched) return false;
		return Date.now() - new Date(peer.last_fetched).getTime() > 7 * 24 * 60 * 60 * 1000;
	}

	// Which groups a contact belongs to (derived from groups[].pubkeys).
	function contactGroups(hb_id: string): string[] {
		return groups.filter(g => g.pubkeys.includes(hb_id)).map(g => g.name);
	}

	// M21 W5b: a group's colour, looked up by name. Returns undefined for a group with no colour (or
	// an unknown name) — the chip renders with no dot in that case, never a broken/invisible chip.
	function groupColor(name: string): string | undefined {
		return groups.find(g => g.name === name)?.color;
	}

	// Per-contact group membership editor (M20 W5, data-loss half). The data model and renderer are
	// many-to-many; this editor is too — checkboxes over ALL groups, pre-checked with the contact's
	// CURRENT memberships, and on Apply the full checked set is sent to contact_update_groups (which
	// diffs the complete set). The old single-select sent `[newGroupName]` and silently dropped every
	// other membership; that live data loss is what this replaces.
	let contactGroupEditing: Record<string, boolean> = $state({});
	// Working checkbox state per contact being edited: hb_id → set of checked group names. Seeded from
	// `contactGroups(hb_id)` when the editor opens so the pre-check never loses existing memberships.
	let contactGroupDraft: Record<string, Set<string>> = $state({});

	function beginGroupEdit(hb_id: string) {
		contactGroupDraft = { ...contactGroupDraft, [hb_id]: new Set(contactGroups(hb_id)) };
		contactGroupEditing = { ...contactGroupEditing, [hb_id]: true };
	}

	function toggleDraftGroup(hb_id: string, name: string, checked: boolean) {
		const next = new Set(contactGroupDraft[hb_id] ?? []);
		if (checked) next.add(name); else next.delete(name);
		// Mutate-in-place then reassign so Svelte 5 sees the change (state[k] = v, not state={...}).
		contactGroupDraft[hb_id] = next;
		contactGroupDraft = { ...contactGroupDraft };
	}

	async function handleSaveGroups(hb_id: string) {
		const groupNames = [...(contactGroupDraft[hb_id] ?? [])];
		try {
			await contactUpdateGroups(hb_id, groupNames);
			await loadGroups();
		} catch (e) { toast(String(e), 'error'); }
		contactGroupEditing = { ...contactGroupEditing, [hb_id]: false };
	}

	// M21 W5b / M22 W8: the `+` on the collapsed card face opens a popover anchored to the chip row,
	// so group membership is reachable without expanding the detail. Same full-set semantics as the
	// expanded editor — Apply sends the complete checked set to contactUpdateGroups, which diffs it.
	// Cancel discards. No confirmation step (safe only because the Private audience moved out in
	// W5a). The popover's draft + checkboxes + focus-return live in the SHARED GroupMembershipPopover
	// component; this page keeps only the open/close state + the full-set write.
	let groupPopoverFor: string | null = $state(null);
	let groupPopoverAnchor: HTMLElement | undefined = $state();

	function openGroupPopover(npub: string, anchor: HTMLElement) {
		// The draft is seeded from CURRENT memberships inside the component (so the pre-check never
		// loses existing memberships — the same data-loss guard the expanded editor enforces).
		groupPopoverAnchor = anchor;
		groupPopoverFor = npub;
	}

	async function applyGroupPopover(npub: string, names: string[]) {
		groupPopoverFor = null;
		try {
			await contactUpdateGroups(npub, names);
			await loadGroups();
		} catch (e) { toast(String(e), 'error'); }
	}

	// M20 W2: the mount fan-out used to fire one full `refreshContact` (= one `resolve_peer`) per
	// contact, ALL in parallel, on EVERY page visit — unbounded. Now capped at REFRESH_CONCURRENCY at
	// a time, and a contact refreshed within the last REFRESH_FRESHNESS_MS is skipped (no resolve at
	// all). A full resolve per contact per page visit is not a refresh policy.
	const REFRESH_CONCURRENCY = 4;
	const REFRESH_FRESHNESS_MS = 10 * 60 * 1000; // 10 min

	onMount(() => {
		loadGroups();
		loadPrivate();
		loadPrivateAudience();
		refreshOnline();
		onlinePollTimer = setInterval(refreshOnline, ONLINE_POLL_VISIBLE_MS);
		// Refresh contacts on page load, but bounded: skip freshly-refreshed contacts and cap how many
		// resolves run at once (task 4 + M20 W2). The pool drains at REFRESH_CONCURRENCY at a time.
		const now = Date.now();
		const stale = $contacts.filter((c) => {
			const fetched = c.last_fetched ? new Date(c.last_fetched).getTime() : 0;
			return now - fetched > REFRESH_FRESHNESS_MS;
		});
		let cursor = 0;
		async function worker() {
			while (cursor < stale.length) {
				const c = stale[cursor++];
				try {
					const updated = await refreshContact(c.npub);
					contacts.update(cs => cs.map(x => x.npub === c.npub ? { ...x, ...updated, local_tags: x.local_tags } : x));
				} catch { /* silent — relay may be unreachable */ }
			}
		}
		const workers = Array.from({ length: Math.min(REFRESH_CONCURRENCY, stale.length) }, worker);
		void Promise.all(workers);
	});

	// Add-contact dialog (M13 W5 Slice 2): both the lookup card and a discovery hit open the same
	// petname + group picker before actually adding — `addContactTarget` is whichever npub is pending.
	let addContactOpen = $state(false);
	let addContactDisplayName = $state('');
	let addContactTarget: string | null = $state(null);
	// The share code `follow` must re-resolve (full `hbk1…` for a lookup, npub for a discovery hit).
	// Kept alongside the npub target so the browse-key isn't dropped in the funnel (devtest #3).
	let addContactCode: string | null = $state(null);
	// M20 W2: the peer the lookup already resolved (`pasteKey`'s result). Held through the dialog and
	// handed to `follow` so it skips a SECOND `resolve_peer` — the fix for "add resolves twice". A
	// discovery hit (lookup = null) carries no pre-resolved peer, so the follow leg resolves as before.
	let addContactResolved: CachedPeer | null = $state(null);

	function openAddContact(code: string, npub: string, displayName: string, resolved: CachedPeer | null) {
		addContactCode = code;
		addContactTarget = npub;
		addContactDisplayName = displayName;
		addContactResolved = resolved;
		addContactOpen = true;
	}

	async function completeFollow(code: string, npub: string, group: string | null, petname: string | undefined, resolved: CachedPeer | null) {
		try {
			await follow(code, group ?? undefined, petname, resolved ?? undefined);
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			await loadGroups();
			toast(`Added ${petname || addContactDisplayName || npub.slice(0, 12) + '…'}`, 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// M17 W1: "Message" on a discovery hit-card (non-contact) → compose deep-link (chat ?compose=).
	// M17 W2: discovery hits are keyless by design, so the Message action always carries the
	// ask-access intent — first contact starts with the right words already in the box.
	function messagePeer(npub: string) {
		goto('/chat?compose=' + npub + '&intent=ask-access');
	}

	async function handleAddContactSave(detail: { petname: string; group: string | null }) {
		if (!addContactTarget || addContactCode === null) return;
		const npub = addContactTarget;
		const code = addContactCode;
		const resolved = addContactResolved;
		addContactOpen = false;
		addContactTarget = null;
		addContactCode = null;
		addContactResolved = null;
		await completeFollow(code, npub, detail.group, detail.petname, resolved);
	}

	async function handleAddContactSkip() {
		if (!addContactTarget || addContactCode === null) return;
		const npub = addContactTarget;
		const code = addContactCode;
		const resolved = addContactResolved;
		addContactOpen = false;
		addContactTarget = null;
		addContactCode = null;
		addContactResolved = null;
		await completeFollow(code, npub, null, undefined, resolved);
	}

	// "+ Add contact" (devtest #17/#18 redesign) — the lookup-by-ID + §6 Discover surfaces now live
	// behind a single centered modal panel instead of cluttering the page. Both of its add entry
	// points call back through `openAddContact`, same funnel as before.
	let addContactPanelOpen = $state(false);

	// Contacts list state (M15 W5: chevron toggles the light-detail area; browsing a peer's
	// collections moved to the Browse tab via the `/browse?peer=` deep-link — no inline expansion).
	let detailExpanded: string | null = $state(null);
	let refreshing: string | null = $state(null);
	let menuOpenFor: string | null = $state(null);
	let menuAnchor: HTMLElement | undefined = $state();
	// M21 W4 — bio "more ⌄" expansion on the card face. Per-npub so each card expands independently.
	// Cleared when the card is opened via `›` (the detail's full bio replaces the clamp; two controls
	// revealing the same text would be redundant).
	let bioExpanded: Record<string, boolean> = $state({});

	function toggleBio(npub: string) {
		bioExpanded[npub] = !bioExpanded[npub];
		bioExpanded = { ...bioExpanded };
	}

	function toggleDetail(npub: string) {
		const opening = detailExpanded !== npub;
		detailExpanded = opening ? npub : null;
		// M21 W4 — opening the detail reveals the full bio there, so the face's `more ⌄` control
		// must collapse (the two controls would otherwise reveal the same text). Reset only on open;
		// closing the detail leaves the bio as-is.
		if (opening && bioExpanded[npub]) {
			bioExpanded[npub] = false;
			bioExpanded = { ...bioExpanded };
		}
	}

	// M23 W6 — bio "more ⌄" overflow detection. The clamp is 2 VISUAL lines; whether a string wraps
	// depends on card width, font and zoom, so this is a MEASUREMENT (scrollHeight > clientHeight on
	// the clamped `.card-bio`), not a character count. Re-evaluated on resize via ResizeObserver so a
	// window-width change that un-wraps the bio drops the control. Per CLAUDE.md §7, mutate
	// bioOverflowMap[npub] directly rather than reassigning a spread object (concurrent updates from
	// multiple cards would otherwise lose entries).
	let bioOverflowMap: Record<string, boolean> = $state({});
	function bioMeasure(row: HTMLElement, npub: string) {
		const bio = row.querySelector<HTMLElement>('.card-bio');
		const measure = () => {
			if (!bio) return;
			bioOverflowMap[npub] = bioOverflows(bio.scrollHeight, bio.clientHeight);
		};
		measure();
		const ro = new ResizeObserver(measure);
		if (bio) ro.observe(bio);
		return { destroy: () => ro.disconnect() };
	}

	// M21 W4 — the npub came off the card face (behaviour 1); "Copy npub" is its surviving home.
	async function copyNpub(npub: string) {
		try {
			await navigator.clipboard.writeText(npub);
			toast('npub copied');
		} catch (e) { toast(String(e), 'error'); }
	}

	function openRowMenu(npub: string, anchor: HTMLElement) {
		menuAnchor = anchor;
		menuOpenFor = npub;
	}

	async function handleRefresh(hb_id: string) {
		refreshing = hb_id;
		try {
			const updated = await refreshContact(hb_id);
			contacts.update((cs) => cs.map((c) => (c.npub === hb_id ? { ...updated, local_tags: c.local_tags } : c)));
			toast('Contact refreshed');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			refreshing = null;
		}
	}

	async function handleUnfollow(hb_id: string) {
		try {
			await unfollowContact(hb_id);
			contacts.update((cs) => cs.filter((c) => c.npub !== hb_id));
			toast('Contact removed');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// M17 W5 — two clocks, two labels. `checkedLabel` is OUR cache age (last_fetched); `presenceView`
	// is THEIR presence, from the newest beacon we hold: this poll's fresh set, else the persisted
	// `last_presence`. The old single "seen {t}" line rendered last_fetched and so said "just now"
	// about someone gone for a week (devtest 2026-07-23 item 2).
	// (The cast restores the declared type: TS narrows a `$state(null)` to `null` at this position,
	// since the only assignment is inside the poll closure.)
	let freshSeen = $derived(freshIndex((onlineData as OnlineCount | null)?.fresh));

	function presenceOf(peer: import('$lib/types.js').CachedPeer): PresenceView & { known: boolean } {
		const seen = newestSeen(peer.npub, freshSeen, peer.last_presence);
		return { ...presenceView(seen, nowMs), known: seen !== null };
	}

	/** A real beacon outranks the stored `online` flag (which a browse stamped once and nobody ever
	 *  clears). With no beacon at all we keep the stored flag rather than flipping a just-browsed
	 *  contact to offline on no evidence. Applied before bucketing so the pill, the "Online now"
	 *  bucket and the header count can never disagree. */
	function withPresence(peer: import('$lib/types.js').CachedPeer): import('$lib/types.js').CachedPeer {
		const p = presenceOf(peer);
		return p.known ? { ...peer, online: p.online } : peer;
	}

	// Tag editing state
	let editingTagsFor: string | null = $state(null);
	let tagInput = $state('');

	async function handleAddTag(hb_id: string, current_tags: string[]) {
		const tag = tagInput.trim();
		if (!tag || current_tags.includes(tag)) { tagInput = ''; return; }
		const newTags = [...current_tags, tag];
		tagInput = '';
		try {
			await setContactTags(hb_id, newTags);
			contacts.update(cs => cs.map(c => c.npub === hb_id ? { ...c, local_tags: newTags } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	async function handleRemoveTag(hb_id: string, current_tags: string[], tag: string) {
		const newTags = current_tags.filter(t => t !== tag);
		try {
			await setContactTags(hb_id, newTags);
			contacts.update(cs => cs.map(c => c.npub === hb_id ? { ...c, local_tags: newTags } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	// Filter by tag — demoted to a collapsible row (default collapsed), applied in both views.
	let filterTag = $state('');
	let tagFilterOpen = $state(false);
	let allTags = $derived([...new Set($contacts.flatMap(c => c.local_tags ?? []))].sort());

	// ── Phonebook redesign (devtest #17/#18): sticky free-text search + Name|Groups view toggle +
	//    a pinned "Online now" bucket (additive — an online peer also still appears in its section). ──
	let searchQuery = $state('');
	let view: 'name' | 'groups' = $state('name');

	// Presence is applied ONCE, to the whole roster, before any filtering — so the header count, the
	// "Online now" bucket and each row's pill are all reading the same adjusted list (W5 review: the
	// header used to count raw `$contacts.online` and could say "0 online" above an Online row).
	let presenced = $derived($contacts.map(withPresence));
	let onlineTotal = $derived(presenced.filter(c => c.online).length);
	let visible = $derived(
		presenced.filter(c => matchesQuery(c, searchQuery)).filter(c => !filterTag || (c.local_tags ?? []).includes(filterTag))
	);
	// #1: an online peer moves OUT of its A-Z section INTO the pinned "Online now" bucket (never both),
	//     and moves back when it goes offline. #8: the Groups view is for organizing, so it has no
	//     Online-now bucket and every group lists all its members (online included).
	let online = $derived(view === 'name' ? onlineBucket(visible) : []);
	let sections = $derived(
		view === 'name' ? groupByLetter(visible.filter((c) => !c.online)) : groupByGroups(visible, groups)
	);
	// M22 W5 — the rendered contact order across all visible sections, so applyClickToSelection
	// can compute the contiguous Shift range. Includes the online bucket in name view.
	let contactOrder = $derived(
		[...online, ...sections.flatMap((s) => s.peers)].map((p) => p.npub),
	);
	// M22 W7 A5 — the GLOBAL render index each section starts at. groupByGroups renders a peer once
	// per group, so a roving tabindex keyed on npub alone would mark every copy tabbable; the render
	// index disambiguates them. online[] is rendered first in name view, hence the initial offset.
	let sectionStart = $derived((() => {
		const out: number[] = [];
		let n = online.length;
		for (const sec of sections) { out.push(n); n += sec.peers.length; }
		return out;
	})());
	let railTargets = $derived(
		ALPHABET.map(l => ({ label: l, anchorId: l === '#' ? 'sec-hash' : `sec-${l}`, enabled: presentSectionKeys(sections).has(l) }))
	);
</script>

<!-- M22 W5 — Esc clears the multi-selection when the naming popover is not open. -->
<!-- M22 W7 — the live region mirrors the drag affordance text so the refuse state (W4) is
     conveyed non-visually, not just by withholding the accent highlight. -->
<svelte:window onkeydown={onWindowKeyDown} />
<div class="sr-only" role="status" aria-live="polite">
	{#if dropOverTarget && dropOutcome}
		{#if dropOutcome.kind === 'refused'}{dropOutcome.reason}{:else if dropOutcome.kind === 'noop'}already ungrouped{:else if dropOutcome.kind === 'add'}add to {dropOverTarget}{:else if dropOutcome.kind === 'move'}move to {dropOverTarget}{:else}remove all groups{/if}
	{/if}
</div>

<div class="contacts-shell">
<div class="contacts-main">
<!-- TopBar -->
<!-- QURATOR-81 follow-up: the topbar IS the title bar, so it is the drag handle. The 8px strip in
     +layout.svelte was the only draggable surface and, on Windows, the top few pixels double as the
     resize hotspot — which left almost nothing to grab and read as "dragging doesn't work" (owner,
     2026-08-13). `data-tauri-drag-region` does NOT inherit to children, so every button and input
     inside this bar keeps its clicks; only the bar's own background drags. -->
<div class="topbar" data-tauri-drag-region>
	<div>
		<div class="topbar-title">Contacts</div>
		<div class="topbar-sub">
			{$contacts.length} contact{$contacts.length !== 1 ? 's' : ''} · {onlineTotal} online
		</div>
	</div>
	{#if chip.show}
		<span class="online-chip online-chip-{chip.state}" class:online-chip-muted={chip.unknown} title={whyHint ? `Hoarders online now — ${whyHint}` : 'Hoarders online now'}><span class="online-dot"></span>{chip.label}</span>
		{#if whyHint}
			<span class="online-why" title={whyHint}>({whyHint})</span>
		{/if}
	{/if}
</div>

<!-- Sticky sub-header: free-text search, Name|Groups view toggle, "+ Add contact" -->
<div class="subheader">
	<div class="hb-input subheader-search">
		<span class="search-icon">{@html icons.search}</span>
		<input type="text" placeholder="Search name, bio, tags, collections…" bind:value={searchQuery} />
	</div>
	<div class="view-toggle" role="group" aria-label="View">
		<button type="button" aria-pressed={view === 'name'} onclick={() => (view = 'name')}>Name</button>
		<button type="button" aria-pressed={view === 'groups'} onclick={() => (view = 'groups')}>Groups</button>
	</div>
	<button type="button" class="btn-primary btn-sm" onclick={() => (addContactPanelOpen = true)}>+ Add contact</button>
</div>

{#if allTags.length > 0}
	<div class="tagfilter-row">
		<button type="button" class="tagfilter-toggle" onclick={() => (tagFilterOpen = !tagFilterOpen)} aria-expanded={tagFilterOpen}>
			Filter by tag <span class="tagfilter-chevron" class:open={tagFilterOpen}>{@html icons.chevronDown}</span>
		</button>
		{#if tagFilterOpen}
			<div class="tag-filter-row">
				<button class="filter-tag" class:filter-tag-active={!filterTag} onclick={() => filterTag = ''}>All</button>
				{#each allTags as tag}
					<button class="filter-tag" class:filter-tag-active={filterTag === tag} onclick={() => filterTag = filterTag === tag ? '' : tag}>{tag}</button>
				{/each}
			</div>
		{/if}
	</div>
{/if}

{#snippet contactRow(peer: CachedPeer, renderIdx: number)}
	{@const name = contactDisplayName(peer)}
	{@const initial = name[0]?.toUpperCase() ?? '?'}
	{@const hue = avatarHue(initial)}
	{@const peerGroups = contactGroups(peer.npub)}
	{@const badge = peerAccessBadge(peer)}
	{@const fp = peer.fingerprint}
	<!-- M21 W4 behaviour 2: presence + cache-age are INDEPENDENT. The pill always answers "are they
	     online" (Stale no longer replaces Offline — it stops reporting presence). The cold-cache marker
	     (isStale && !online) renders in the meta line as an amber "checked {t} ↻" control, not a verdict. -->
	{@const coldCache = isStale(peer) && !peer.online}
	<!-- M21 W4 behaviour 6: public collections only — the popover list AND the face count read the same
	     filtered set so the number and the list can never disagree (a disclosure boundary, not cosmetic). -->
	{@const publicCollections = peer.collections.filter(c => c.visibility !== 'Private')}
	{@const pubCount = publicCollections.length}
	{@const sizeSummary = !badge.locked ? summarizeCollectionsSize(publicCollections) : null}
	<!-- M21 W4 behaviour 5: the petnameFor collision warning goes on the card face. The fingerprint is
	     ~36 bits and grindable, so this red-outlined badge is the actual security control. -->
	{@const contactsForLabel = $contacts.map(c => ({ npub: c.npub, petname: c.petname ?? c.profile?.display_name ?? '' }))}
	{@const collision = petnameFor(peer.npub, name, contactsForLabel).warning}
	{@const bio = peer.profile?.bio?.trim()}
	{@const bioOpen = bioExpanded[peer.npub] ?? false}
	{@const isOpen = detailExpanded === peer.npub}
	<div class="contact-block" class:open={isOpen}>
		<!-- devtest v0.12.1 #4: double-click a contact to open the conversation in Chat. The chevron,
		     Browse, and ⋯ controls keep their own single-click actions. -->
		<!-- M22 W3: the contact-card is also a drag source AND a drop target for drag-to-group.
		     dragstart writes the source npub; dragover lights the target + states the outcome;
		     drop opens the naming popover. -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<!-- Ignore double-clicks that land on an inner control (chevron, Browse, ⋯ menu) so they keep
		     their own single-click action instead of also navigating to Chat (codex review). -->
		<!-- M22 W7 — role/aria + roving tabindex: one row in the list owns tabindex=0 (the focused
		     one, or the first when none is focused yet); the rest carry -1 so arrow keys move a
		     single tab stop through the list rather than tabbing through every row. -->
		<div
			class="contact-card"
			class:contact-selected={selectedNpubSet.has(peer.npub)}
			class:drag-source={dragSourceNpub === peer.npub}
			class:drag-target={dragSourceNpub !== null && dragOverNpub === peer.npub && dragSourceNpub !== peer.npub}
			class:drag-recede={dragSourceNpub !== null && dragOverNpub !== null && dragSourceNpub !== peer.npub && dragOverNpub !== peer.npub}
			draggable="true"
			role="option"
			aria-selected={selectedNpubSet.has(peer.npub)}
			aria-label={`${name}${peerGroups.length > 0 ? ', groups: ' + peerGroups.join(', ') : ''}${selectedNpubSet.has(peer.npub) ? ', selected' : ''}`}
			id={rowId(ROW_ID_PREFIX, peer.npub)}
			tabindex={rovingTabindexForIdx(focusedIdx, renderIdx, contactOrder.length)}
			onfocus={() => onRowFocus(peer.npub)}
			onmousedown={(e) => onContactMouseDown(e, peer.npub)}
			ondragstart={(e) => onDragStart(e, peer.npub)}
			ondragover={(e) => onDragOver(e, peer.npub)}
			ondragleave={() => onDragLeave(peer.npub)}
			ondrop={(e) => onDrop(e, peer.npub)}
			ondragend={onDragEnd}
			ondblclick={(e) => { if ((e.target as HTMLElement).closest('button, a')) return; goto('/chat?peer=' + peer.npub); }}
			title="Double-click to message in Chat"
		>
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<button class="chevron-btn" onclick={() => toggleDetail(peer.npub)} aria-expanded={isOpen} aria-label="Toggle details">
				<span class="chevron" class:chevron-open={isOpen}>{@html icons.chevronDown}</span>
			</button>
			<div class="avatar-wrap" style={fp ? `box-shadow: 0 0 0 2px var(--bg-elev1), 0 0 0 4px ${fp.colorHex}` : undefined}>
				<Avatar letter={initial} size={34} {hue} picture={peer.profile?.picture} />
				{#if badge.locked}
					<span class="lock-overlay" title={badge.hint}>🔒</span>
				{/if}
			</div>
			<div class="contact-info">
				<!-- Row 1: name · presence pill · (offline) seen · spacer · actions.
				     M21 W4 behaviour 1: the npub is OFF the face (the pill takes its slot). The npub
				     survives in the ⋯ menu ("Copy npub") and in the expanded detail. -->
				<div class="name-row">
					<span class="peer-name">{name}</span>
					{#if collision}
						<span class="collision-badge" title={collision}>⚠ {collision}</span>
					{/if}
					{#if peer.online}
						<span class="pill pill-online"><span class="pill-dot"></span></span>
					{:else}
						<span class="pill pill-offline">Offline</span>
					{/if}
					<!-- W5: THEIR presence (only when offline — the pill already says "online"). -->
					{#if !peer.online}
						<span class="last-seen">{presenceOf(peer).lastSeen}</span>
					{/if}
					<div style="flex:1"></div>
					{#if badge.locked}
						<button class="btn-default btn-xs ask-access-btn" onclick={() => goto('/chat?peer=' + peer.npub + '&intent=ask-access' + (peer.petname ? '&petname=' + encodeURIComponent(peer.petname) : ''))}>Ask for access</button>
					{:else}
						<a class="btn-default btn-xs" href="/browse?peer={peer.npub}">Browse</a>
					{/if}
					<button class="btn-default btn-xs" onclick={() => goto('/chat?peer=' + peer.npub)}>Message</button>
					<button
						class="row-menu-btn"
						aria-label="Contact actions"
						aria-haspopup="true"
						aria-expanded={menuOpenFor === peer.npub}
						onclick={(e) => openRowMenu(peer.npub, e.currentTarget)}
					>⋯</button>
				</div>
				<!-- M22 W3 — the Aim moment: when this row is the active drop target, state the outcome in
				     words (not an icon). Other rows recede (handled by .drag-recede on the card). -->
				{#if dragSourceNpub !== null && dragOverNpub === peer.npub && dragSourceNpub !== peer.npub}
					<!-- M22 W5: the Aim text reflects the drag size — "group these two" for a W3 single-pair
					     drag, "group N contacts" for a multi-select drag of N (including the target). -->
					<div class="drag-outcome">{dragCount > 0 ? `group ${dragCount + 1} contacts` : 'group these two'}</div>
				{/if}
				<!-- Row 2: coloured fingerprint (behaviour 3). Absent for a pre-fingerprint stored
				     contact (behaviour 4: no ring, no word row, card otherwise unchanged). -->
				{#if fp}
					<div class="fp-row">
						{#each fp.words as w, i}
							{#if i > 0}<span class="fp-sep">·</span>{/if}
							<span class="fp-word" style={fingerprintWordColor(w) ? `color:${fingerprintWordColor(w)}` : undefined}>{w}</span>
						{/each}
					</div>
				{/if}
				<!-- Row 3: bio, clamped to 2 lines with a `more ⌄` control (behaviour 7: the detail's
				     duplicate bio paragraph is gone, so this clamp is the only place the bio previews). -->
				{#if bio}
					<div class="bio-row" class:bio-expanded={bioOpen || isOpen} use:bioMeasure={peer.npub}>
						<p class="card-bio">{bio}</p>
						{#if !bioOpen && !isOpen && bioOverflowMap[peer.npub]}
							<button class="bio-more" onclick={() => toggleBio(peer.npub)}>more ⌄</button>
						{/if}
					</div>
				{:else}
					<div class="bio-row bio-empty"><span class="no-bio">No bio published.</span></div>
				{/if}
				<!-- Row 4: group chips · N collections (hoverable, public only) · size · cold-cache.
				     M22 W4: each chip is ALSO a drop target (nearer than the section heading).
				     stopPropagation on dragover/drop so the card's W3 create-gesture doesn't also fire. -->
				<div class="contact-sub-row">
					{#each peerGroups as gname (gname)}
						{@const gcolor = groupColor(gname)}
						<span
							class="group-pill"
							class:group-drop-active={dropOverTarget === gname && dropOutcome && dropOutcome.kind !== 'refused' && dropOutcome.kind !== 'noop'}
							class:group-drop-refused={dropOverTarget === gname && dropOutcome && (dropOutcome.kind === 'refused' || dropOutcome.kind === 'noop')}
							ondragover={(e) => { e.stopPropagation(); onGroupDragOver(e, gname); }}
							ondragleave={(e) => { e.stopPropagation(); onGroupDragLeave(gname); }}
							ondrop={(e) => { e.stopPropagation(); onGroupDrop(e, gname); }}
						>
							{#if gcolor}<span class="group-dot" style={`background:${gcolor}`}></span>{/if}
							{gname}
						</span>
					{/each}
					<!-- M21 W5b: `+` opens a membership popover anchored to this row (no detail expand
					     needed). Full-set Apply — the same command the expanded editor uses. -->
					{#if groups.length > 0}
						<button
							class="group-add-btn"
							aria-label="Edit {name}'s groups"
							aria-haspopup="true"
							aria-expanded={groupPopoverFor === peer.npub}
							onclick={(e) => openGroupPopover(peer.npub, e.currentTarget)}
						>+</button>
					{/if}
					{#if peerGroups.length > 0 || groups.length > 0}<span class="sub-dot">·</span>{/if}
					{#if pubCount > 0}
						<span class="sub-dot">·</span>
						<!-- M21 W4 behaviour 6: `▼` caret signals hoverable; :focus-within makes it
						     keyboard-reachable too; public collections ONLY (private ones live in the
						     detail, never in this popover or this count). -->
						<span class="collections-popover-wrap" tabindex="0" role="button" aria-label="Show public collections">
							<span class="sub-meta collections-trigger">{pubCount} collection{pubCount !== 1 ? 's' : ''} ▼</span>
							<div class="collections-popover" role="dialog">
								{#each publicCollections as col (col.slug)}
									<div class="cpop-row" title={col.description ?? ''}>
										<span class="cpop-alias">{col.path_alias}</span>
										<span class="cpop-size">{col.est_size ?? '—'}</span>
										<span class="cpop-desc">{col.description ?? 'No description'}</span>
									</div>
								{/each}
								<div class="cpop-footer">public collections only — private ones live in the detail</div>
							</div>
						</span>
					{/if}
					{#if !badge.locked}
						{#if sizeSummary}<span class="sub-dot">·</span><span class="sub-meta">{sizeSummary}</span>
						{:else if peer.profile?.est_size}<span class="sub-dot">·</span><span class="sub-meta">~{peer.profile.est_size}</span>{/if}
					{/if}
					<!-- Cold cache: an amber "checked {t} ↻" CONTROL (clickable to refresh), only when
					     isStale && !online. When the cache is warm this element is absent from the card. -->
					{#if coldCache}
						<span class="sub-dot">·</span>
						<button class="cold-cache" onclick={() => handleRefresh(peer.npub)} disabled={refreshing === peer.npub} title="Refetch this contact">{checkedLabel(peer.last_fetched, nowMs)} ↻</button>
					{/if}
				</div>
			</div>
		</div>

		{#if isOpen}
			<div class="contact-detail">
				<!-- M21 W4 behaviour 7: the detail's OWN bio paragraph is GONE (the clamped face bio
				     replaced it — keeping both would duplicate the same text behind two controls). -->
				{#if (peer.profile?.content_types?.length ?? 0) > 0}
					<div class="badge-row-sm">
						{#each peer.profile?.content_types ?? [] as ct (ct)}
							<span class="ct-badge">{ct}</span>
						{/each}
					</div>
				{/if}
				{#if (peer.profile?.tags?.length ?? 0) > 0}
					<div class="peer-tags">
						{#each peer.profile?.tags ?? [] as tag (tag)}
							<span class="peer-tag">{tag}</span>
						{/each}
					</div>
				{/if}

				<!-- Groups -->
				{#if peerGroups.length > 0 || contactGroupEditing[peer.npub]}
					<div class="group-row">
						{#if contactGroupEditing[peer.npub]}
							<!-- M20 W5: multi-select checkbox editor. Every group is listed with a checkbox
							     pre-checked to its current membership; on Apply the full checked set is sent to
							     contact_update_groups. This replaces the single-select that silently dropped all
							     other memberships (live data loss). -->
							<div class="group-edit-list">
								{#each groups as g (g.name)}
									<label class="group-check">
										<input
											type="checkbox"
											checked={peerGroups.includes(g.name)}
											onchange={(e) => toggleDraftGroup(peer.npub, g.name, e.currentTarget.checked)}
										/>
										{#if g.color}<span class="group-dot" style={`background:${g.color}`}></span>{/if}
										{g.name}
									</label>
								{/each}
								{#if groups.length === 0}
									<span class="group-edit-empty">No groups yet.</span>
								{/if}
							</div>
							<button class="tag-x" onclick={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: false }; }}>×</button>
							<button class="btn-primary btn-xs" onclick={() => handleSaveGroups(peer.npub)}>Apply</button>
						{:else}
							{#each peerGroups as gname (gname)}
								{@const gcolor = groupColor(gname)}
								<span class="group-pill">
									{#if gcolor}<span class="group-dot" style={`background:${gcolor}`}></span>{/if}
									{gname}
								</span>
							{/each}
							{#if groups.length > 0}
								<button class="tag-add-btn" onclick={() => beginGroupEdit(peer.npub)}>
									{peerGroups.length > 0 ? '✎' : '+ group'}
								</button>
							{/if}
						{/if}
					</div>
				{:else if groups.length > 0}
					<div class="group-row">
						<button class="tag-add-btn" onclick={() => beginGroupEdit(peer.npub)}>+ group</button>
					</div>
				{/if}

				<!-- Local tags -->
				<div class="tag-row">
					{#each (peer.local_tags ?? []) as tag}
						<span class="local-tag">
							{tag}
							<button class="tag-x" onclick={() => handleRemoveTag(peer.npub, peer.local_tags ?? [], tag)}>×</button>
						</span>
					{/each}
					{#if editingTagsFor === peer.npub}
						<input
							class="hb-input tag-input"
							type="text"
							placeholder="tag…"
							bind:value={tagInput}
							onkeydown={(e) => {
								if (e.key === 'Enter' || e.key === ',') { e.preventDefault(); handleAddTag(peer.npub, peer.local_tags ?? []); }
								if (e.key === 'Escape') { editingTagsFor = null; tagInput = ''; }
							}}
							onblur={() => { editingTagsFor = null; tagInput = ''; }}
						/>
					{:else}
						<button class="tag-add-btn" onclick={() => { editingTagsFor = peer.npub; tagInput = ''; }}>+ tag</button>
					{/if}
				</div>

				<!-- M21 W5: the Private-collection audience is a per-contact toggle, decoupled from
				     groups (owner ruling 2026-08-04). Ticking it seals every Private collection to
				     this contact on the next publish; unticking revokes on the next republish only. -->
				<div class="group-row">
					<label class="audience-check">
						<input
							type="checkbox"
							checked={receivesPrivate(peer.npub, privateAudience)}
							onchange={() => toggleReceivesPrivate(peer.npub)}
						/>
						Receives my Private collections
					</label>
				</div>

				<!-- Private collections sealed to me (M10). QURATOR-92: Browse now serves them in a
				     badged Private section via the same `?peer=` deep-link the row's Browse button
				     uses, so the label is a real link; the detail panels stay here. Absent (not
				     "locked") for a non-trusted viewer. -->
				{#if (privateByAuthor[peer.npub] ?? []).length > 0}
					<div class="private-section">
						<div class="section-label">
							<a class="private-link" href="/browse?peer={peer.npub}">Private collections</a>
							<span class="private-badge">trusted</span>
						</div>
						{#each privateByAuthor[peer.npub] as col}
							<CollectionPanel collection={col} />
						{/each}
						<p class="not-drm-note">{NOT_DRM_NOTE}</p>
					</div>
				{/if}
			</div>
		{/if}
	</div>

	<!-- M15 W5: per-row overflow menu (only one open at a time via menuOpenFor). -->
	<OverflowMenu open={menuOpenFor === peer.npub} anchor={menuAnchor} onclose={() => (menuOpenFor = null)}>
		<button class="menu-item" onclick={() => { goto('/chat?peer=' + peer.npub); menuOpenFor = null; }}>Message</button>
		<button class="menu-item" onclick={() => { copyNpub(peer.npub); menuOpenFor = null; }}>Copy npub</button>
		<button class="menu-item" onclick={() => { handleRefresh(peer.npub); menuOpenFor = null; }} disabled={refreshing === peer.npub}>
			{refreshing === peer.npub ? 'Refreshing…' : 'Refresh'}
		</button>
		<button class="menu-item" onclick={() => { detailExpanded = peer.npub; beginGroupEdit(peer.npub); menuOpenFor = null; }}>Edit groups…</button>
		<button class="menu-item" onclick={() => { detailExpanded = peer.npub; editingTagsFor = peer.npub; tagInput = ''; menuOpenFor = null; }}>Edit tags…</button>
		<div class="menu-item menu-item-confirm">
			<ConfirmButton label="Remove contact" confirmText="Remove this contact?" onconfirm={() => { handleUnfollow(peer.npub); menuOpenFor = null; }} />
		</div>
	</OverflowMenu>

	<!-- M21 W5b / M22 W8: group-membership popover anchored to the face chip row. The editor is the
	     ONE GroupMembershipPopover component (used by Browse too) — OverflowMenu positioning +
	     Escape, seeded draft, full-set Apply via contactUpdateGroups, "+ New group…", and focus-return
	     to the invoking row all live there. This page supplies only the anchor + callbacks. -->
	<GroupMembershipPopover
		open={groupPopoverFor === peer.npub}
		anchor={groupPopoverAnchor}
		contactName={name}
		groups={groups}
		memberships={contactGroups(peer.npub)}
		ready={groupsLoaded}
		onapply={(names) => applyGroupPopover(peer.npub, names)}
		onclose={() => (groupPopoverFor = null)}
		onnewgroup={() => (createGroupOpen = true)}
		returnFocusTo={() => {
			const el = document.getElementById(rowId(ROW_ID_PREFIX, peer.npub));
			return el?.isConnected ? el : undefined;
		}}
	/>
{/snippet}

<div class="phonebook" bind:this={listContainer}>
	<div class="phonebook-scroll">
		{#if $contacts.length === 0}
			<div class="empty">No contacts yet. Use “+ Add contact” to find someone by ID or discover hoarders.</div>
		{:else}
			{#if view === 'groups'}
				<!-- Groups-view management strip (M21 W5): organisational only — colour is the sole
				     marker, no trust checkbox (decoupled from Private collections by owner ruling
				     2026-08-04). Delete moves members to Ungrouped; "+ New group" stays reachable
				     here so the groups view is self-sufficient, not just from a contact's picker. -->
				<div class="group-strip">
					<div class="group-strip-label">Groups</div>
					<div class="group-strip-chips">
						{#each groups as g (g.name)}
							<span class="group-chip-wrap">
								<span class="group-chip" style={g.color ? `border-color:${g.color}; color:${g.color}` : ''}>
									{g.name}
								</span>
								<ConfirmButton
									label="×"
									confirmText={`Delete "${g.name}"? Members fall back to Ungrouped.`}
									onconfirm={() => handleDeleteGroup(g)}
								/>
							</span>
						{/each}
						<button type="button" class="group-chip group-chip-add" onclick={() => (createGroupOpen = true)}>+ New group</button>
					</div>
				</div>
			{/if}

			{#if sections.length === 0 && online.length === 0}
				{#if searchQuery.trim()}
					<div class="empty">No contacts match "{searchQuery}".</div>
				{:else if filterTag}
					<div class="empty">No contacts with tag "{filterTag}".</div>
				{/if}
			{:else}
				{#if online.length > 0}
					<div class="phonebook-section">
						<div id="sec-online" class="section-header">● Online now</div>
						<div class="contact-list">
							{#each online as peer, i (peer.npub)}
								{@render contactRow(peer, i)}
							{/each}
						</div>
					</div>
				{/if}
				{#each sections as section, secIdx (section.key)}
					{@const dropTargetName = section.key === 'ungrouped' ? UNGROUPED_TARGET : section.key}
					<div class="phonebook-section">
						<div
							id={section.anchorId}
							class="section-header"
							class:group-drop-active={dropOverTarget === dropTargetName && dropOutcome && dropOutcome.kind !== 'refused' && dropOutcome.kind !== 'noop'}
							class:group-drop-refused={dropOverTarget === dropTargetName && dropOutcome && (dropOutcome.kind === 'refused' || dropOutcome.kind === 'noop')}
							ondragover={(e) => onGroupDragOver(e, dropTargetName)}
							ondragleave={() => onGroupDragLeave(dropTargetName)}
							ondrop={(e) => onGroupDrop(e, dropTargetName)}
							role={view === 'groups' ? 'group' : undefined}
							aria-label={view === 'groups' ? section.label : undefined}
							aria-disabled={dropOverTarget === dropTargetName && dropOutcome !== null && (dropOutcome.kind === 'refused' || dropOutcome.kind === 'noop')}
						>
							{section.label}
							{#if view === 'groups' && dropOverTarget === dropTargetName && dropOutcome}
								<span class="drop-hint">{#if dropOutcome.kind === 'refused'}{dropOutcome.reason}{:else if dropOutcome.kind === 'noop'}already ungrouped{:else if dropOutcome.kind === 'add'}add to {section.label}{:else if dropOutcome.kind === 'move'}move to {section.label}{:else}remove all groups{/if}</span>
							{/if}
						</div>
						<div class="contact-list">
							{#each section.peers as peer, i (peer.npub)}
								{@render contactRow(peer, sectionStart[secIdx] + i)}
							{/each}
						</div>
					</div>
				{/each}
			{/if}
		{/if}
	</div>
	{#if view === 'name'}
		<AZRail targets={railTargets} />
	{/if}
</div>
</div>
</div>

<CreateGroupDialog open={createGroupOpen} oncreate={handleCreateGroup} oncancel={() => (createGroupOpen = false)} />
<AddContactPanel
	open={addContactPanelOpen}
	onadd={openAddContact}
	onmessage={messagePeer}
	onclose={() => (addContactPanelOpen = false)}
/>
<AddContactDialog
	open={addContactOpen}
	displayName={addContactDisplayName}
	{groups}
	onsave={handleAddContactSave}
	onskip={handleAddContactSkip}
	onnewGroup={() => (createGroupOpen = true)}
	oncancel={() => { addContactOpen = false; addContactTarget = null; addContactResolved = null; }}
/>

<!-- M22 W3 — the Name moment: a popover anchored at the drop point with the two avatars
     overlapping, a focused text field ("Name this group"), and up to 3 suggestion chips.
     Esc cancels the whole gesture (no group created). Enter commits (additive — both peers
     keep every group they were already in and both gain the new one).
     M22 W5 — for a multi-select drop, the two avatars are replaced by a count badge ("N"). -->
{#if dragPopoverFor}
	{@const isMulti = Array.isArray(dragPopoverFor)}
	{@const dragSourcePeer = !isMulti ? $contacts.find((c) => c.npub === (dragPopoverFor as { source: string; target: string })!.source) : undefined}
	{@const dragTargetPeer = !isMulti ? $contacts.find((c) => c.npub === (dragPopoverFor as { source: string; target: string })!.target) : undefined}
	<OverflowMenu open={dragPopoverFor !== null} anchor={dragPopoverAnchor} onclose={closeDragPopover} minWidth="260px">
		<div class="dg-header">
			<div class="dg-avatars">
				{#if isMulti}
					<span class="dg-count" title={`${(dragPopoverFor as string[]).length} contacts`}>{(dragPopoverFor as string[]).length}</span>
				{:else}
					{#if dragSourcePeer}
						{@const dgInitial = (contactDisplayName(dragSourcePeer)[0] ?? '?').toUpperCase()}
						<span class="dg-avatar dg-avatar-a" style={`--dg-hue:${avatarHue(dgInitial)}`}>
							{dgInitial}
						</span>
					{/if}
					{#if dragTargetPeer}
						{@const dgInitial = (contactDisplayName(dragTargetPeer)[0] ?? '?').toUpperCase()}
						<span class="dg-avatar dg-avatar-b" style={`--dg-hue:${avatarHue(dgInitial)}`}>
							{dgInitial}
						</span>
					{/if}
				{/if}
			</div>
			<input
				class="hb-input dg-input"
				type="text"
				placeholder="Name this group"
				bind:this={dragNameEl}
				bind:value={dragNameInput}
				onkeydown={onDragNameKey}
			/>
		</div>
		{#if dragSuggestions.length > 0}
			<div class="dg-suggestions">
				{#each dragSuggestions as s (s)}
					<button type="button" class="dg-chip" onclick={() => pickSuggestion(s)}>{s}</button>
				{/each}
			</div>
		{/if}
		<div class="dg-footer">
			<button type="button" class="btn-ghost btn-xs" onclick={closeDragPopover}>Cancel</button>
			<button type="button" class="btn-primary btn-xs" disabled={dragNameInput.trim().length === 0} onclick={commitDragCreate}>Create</button>
		</div>
	</OverflowMenu>
{/if}

<style>
	.contacts-shell {
		display: flex;
		flex: 1;
		overflow: hidden;
		min-width: 0;
	}
	.contacts-main {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

	.topbar {
		padding: 16px 24px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		background: var(--bg);
		flex-shrink: 0;
	}
	.topbar-title { font-size: 17px; font-weight: 600; letter-spacing: -0.3px; }
	.topbar-sub { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }
	.online-chip {
		display: inline-flex; align-items: center; gap: 5px;
		font-size: 12px; font-weight: 600; color: var(--fg-dim); white-space: nowrap;
	}
	.online-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; display: inline-block; }
	.online-chip-red .online-dot { background: var(--error); }
	.online-chip-amber .online-dot { background: oklch(0.78 0.15 75); }
	.online-chip-green .online-dot { background: var(--online); }
	.online-chip-muted { opacity: 0.55; }
	.online-why { font-size: 10.5px; color: var(--fg-dim); opacity: 0.7; white-space: nowrap; }

	/* Sticky sub-header: search + view toggle + add-contact — outside the scroll container. */
	.subheader {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 24px;
		border-bottom: 1px solid var(--border);
		background: var(--bg);
		flex-shrink: 0;
	}
	/* QURATOR-52 §5 — on the .hb-input contract; only the search-specific layout (icon gap,
	   max-width, padding for the icon prefix) is local. The inner input is transparent/borderless
	   so the wrapper reads as one search field. */
	.subheader-search {
		flex: 1;
		max-width: 380px;
		gap: 8px;
		padding: 0 11px;
	}
	.subheader-search .search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }
	.subheader-search input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 13px;
		color: var(--fg);
		min-width: 0;
	}
	.subheader-search input::placeholder { color: var(--fg-dim); }

	.view-toggle {
		display: flex;
		border: 1px solid var(--border);
		border-radius: 7px;
		overflow: hidden;
		flex-shrink: 0;
	}
	.view-toggle button {
		padding: 6px 12px;
		font-size: 12px;
		font-weight: 600;
		background: transparent;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		font-family: var(--font-ui);
	}
	.view-toggle button[aria-pressed='true'] { background: var(--accent-soft); color: var(--accent); }

	/* Tag filter — collapsible, under the search bar, applied in both views. */
	.tagfilter-row { padding: 8px 24px 0; flex-shrink: 0; }
	.tagfilter-toggle {
		background: transparent; border: none; cursor: pointer;
		color: var(--fg-dim); font-size: 11px; font-weight: 500;
		display: inline-flex; align-items: center; gap: 4px;
		font-family: var(--font-ui); padding: 2px 0;
	}
	.tagfilter-toggle:hover { color: var(--fg-muted); }
	.tagfilter-chevron { display: flex; transition: transform 0.15s; }
	.tagfilter-chevron.open { transform: rotate(180deg); }

	.section-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
	}

	/* M21 W5 — groups-view management strip (organisational, no trust) + per-contact
	   Private-audience toggle + private-collection display. The strip CSS is the old trusted-strip
	   CSS renamed off the `trusted-` prefix, minus `.is-trusted` (trust is gone). */
	.group-strip { padding: 8px 0 16px; display: flex; flex-direction: column; gap: 6px; }
	.group-strip-label { font-size: 11.5px; font-weight: 600; color: var(--fg-muted); }
	.group-strip-chips { display: flex; flex-wrap: wrap; gap: 6px; }
	.group-chip-wrap { display: inline-flex; align-items: center; gap: 2px; }
	.group-chip {
		display: inline-flex; align-items: center; gap: 5px;
		padding: 3px 9px; border: 1px solid transparent; border-radius: 999px;
		font-size: 12px; color: var(--fg-muted);
	}
	.group-chip-add {
		background: transparent; border-style: dashed; cursor: pointer;
		font-family: var(--font-ui); color: var(--fg-dim);
	}
	.group-chip-add:hover { border-color: var(--accent); color: var(--accent); }
	.audience-check {
		display: inline-flex; align-items: center; gap: 4px;
		font-size: 11.5px; color: var(--fg-muted); cursor: pointer;
	}
	.audience-check input { margin: 0; cursor: pointer; }
	.private-section { margin-top: 10px; display: flex; flex-direction: column; gap: 8px; }
	.private-link { color: inherit; text-decoration: none; }
	.private-link:hover { color: var(--accent); text-decoration: underline; }
	.private-badge {
		font-size: 9.5px; padding: 1px 6px; border-radius: 999px; letter-spacing: 0.5px;
		background: color-mix(in oklch, var(--accent) 16%, transparent); color: var(--accent);
	}
	.not-drm-note { margin: 2px 0 0; font-size: 11px; line-height: 1.4; color: var(--fg-dim); }

	/* Phonebook: scrollable section list + a fixed A-Z rail sibling. */
	.phonebook { display: flex; min-height: 0; flex: 1; max-width: 760px; }
	.phonebook-scroll { flex: 1; overflow-y: auto; padding: 16px 24px 24px; min-width: 0; }

	.phonebook-section { margin-bottom: 4px; }
	.section-header {
		position: sticky; top: 0; z-index: 2;
		background: var(--bg);
		padding: 6px 0;
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 700;
		scroll-margin-top: 0;
	}
	#sec-online.section-header { color: var(--online); }

	/* Contacts list */
	.empty { color: var(--fg-dim); font-size: 13px; padding: 16px 0; }

	.contact-list { display: flex; flex-direction: column; gap: 12px; padding-bottom: 16px; }

	.contact-block { display: flex; flex-direction: column; gap: 8px; scroll-margin-top: 34px; }
	/* devtest v0.12.4 #6: an expanded row reads as ONE connected card — the detail continues the card
	   body (shared surface + border, squared seam) instead of floating as an unstyled block below it. */
	.contact-block.open { gap: 0; }
	.contact-block.open .contact-card {
		border-bottom-left-radius: 0;
		border-bottom-right-radius: 0;
		border-bottom-color: transparent;
	}

	.contact-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 10px 12px;
		display: flex;
		gap: 10px;
		align-items: flex-start;
	}

	/* M15 W5: chevron toggles the light-detail area; ⋯ opens the row menu. */
	.chevron-btn {
		background: transparent; border: none; cursor: pointer; padding: 2px;
		display: flex; align-items: center; color: var(--fg-muted); flex-shrink: 0; margin-top: 5px;
	}
	.chevron { display: flex; transition: transform 0.15s; }
	.chevron-open { transform: rotate(180deg); }
	.row-menu-btn {
		width: 26px; height: 26px; flex-shrink: 0;
		display: flex; align-items: center; justify-content: center;
		background: transparent; border: 1px solid transparent; border-radius: 6px;
		color: var(--fg-muted); font-size: 15px; line-height: 1; cursor: pointer;
	}
	.row-menu-btn:hover { background: var(--bg-elev3); color: var(--fg); }
	.menu-item {
		display: flex; align-items: center; width: 100%; text-align: left;
		padding: 7px 10px; font-family: var(--font-ui); font-size: 12.5px; color: var(--fg);
		background: transparent; border: none; border-radius: 5px; cursor: pointer;
	}
	.menu-item:hover:not(:disabled) { background: var(--bg-elev3); }
	.menu-item:disabled { opacity: 0.6; cursor: default; }
	.menu-item-confirm { padding: 3px 6px; }

	.contact-info { flex: 1; min-width: 0; }

	.name-row { display: flex; gap: 8px; align-items: center; margin-bottom: 3px; flex-wrap: wrap; }
	.peer-name { font-weight: 600; font-size: 15px; letter-spacing: -0.2px; }

	.last-seen { font-size: 10.5px; color: var(--fg-dim); }

	.contact-sub-row {
		display: flex; align-items: center; gap: 5px;
		margin-top: 6px; font-size: 11px; color: var(--fg-muted);
	}
	.sub-dot { color: var(--fg-dim); }
	.sub-meta { color: var(--fg-muted); font-feature-settings: 'tnum'; }

	/* Browse-key access (devtest #1/#6) — a 🔒 overlay on the avatar for a keyless (locked) contact
	   replaces the old inline "key needed"/"browseable" text badge; keyed contacts get no marker. */
	.avatar-wrap { position: relative; flex-shrink: 0; margin-top: 2px; }
	.lock-overlay {
		position: absolute; right: -4px; bottom: -4px;
		font-size: 10px; line-height: 1;
		padding: 2px; border-radius: 999px;
		background: var(--bg-elev1);
		box-shadow: 0 0 0 1px var(--border);
	}
	/* M17 W2: the locked contact card's "Ask for access" affordance turns the dead-end hint into a
	   next step → the chat ask-access deep-link (no wire change, just a prefilled draft). */
	.ask-access-btn { flex-shrink: 0; }

	/* Contact-card bio (devtest #7) — render-only, clamped to 2 lines. */
	.card-bio {
		font-size: 12px; color: var(--fg-muted); line-height: 1.5;
		margin: 4px 0 0;
		overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical;
	}

	/* M21 W4 — fingerprint word row + collision badge + bio clamp control + collections popover.
	   The .card-bio above keeps its 2-line clamp; `.bio-expanded .card-bio` lifts the clamp so the
	   `more ⌄` control reveals the full text in place. */
	.fp-row { display: flex; gap: 4px; align-items: baseline; margin-top: 2px; flex-wrap: wrap; }
	.fp-word { font-family: var(--font-mono); font-size: 11px; font-weight: 600; }
	.fp-sep { color: var(--fg-dim); font-size: 11px; }
	.collision-badge {
		font-size: 10px; padding: 1px 6px; border-radius: 4px;
		border: 1px solid color-mix(in oklch, var(--error) 60%, transparent);
		color: var(--error); font-weight: 500;
	}
	.bio-row { display: flex; gap: 6px; align-items: flex-end; margin-top: 2px; }
	.bio-row .card-bio { margin: 0; }
	.bio-expanded .card-bio { -webkit-line-clamp: unset; line-clamp: unset; overflow: visible; }
	.bio-more {
		background: transparent; border: none; cursor: pointer; flex-shrink: 0;
		font-size: 10.5px; color: var(--fg-dim); padding: 0 0 2px 0; font-family: var(--font-ui);
		white-space: nowrap;
	}
	.bio-more:hover { color: var(--fg-muted); }
	.bio-empty { font-size: 12px; }
	.no-bio { color: var(--fg-dim); font-style: italic; font-size: 11.5px; }

	/* M21 W4 behaviour 6 — `N collections` hoverable. ▼ caret signals it; :focus-within makes it
	   keyboard-reachable. Public collections ONLY (disclosure boundary). */
	.collections-popover-wrap { position: relative; display: inline-flex; }
	.collections-trigger { cursor: default; }
	.collections-popover {
		display: none; position: absolute; top: 100%; left: 0; z-index: var(--z-menu);
		min-width: 220px; margin-top: 6px;
		background: var(--bg-elev2); border: 1px solid var(--border); border-radius: 8px;
		padding: 6px; box-shadow: 0 8px 24px oklch(0 0 0 / 0.3);
	}
	.collections-popover-wrap:hover .collections-popover,
	.collections-popover-wrap:focus-within .collections-popover { display: block; }
	.cpop-row { display: flex; flex-direction: column; gap: 1px; padding: 4px 6px; border-radius: 5px; }
	.cpop-row:hover { background: var(--bg-elev3); }
	.cpop-alias { font-size: 12px; font-weight: 500; color: var(--fg); }
	.cpop-size { font-size: 10.5px; color: var(--fg-muted); font-feature-settings: 'tnum'; }
	.cpop-desc { font-size: 11px; color: var(--fg-muted); }
	.cpop-row:hover .cpop-desc { color: var(--fg); }
	.cpop-footer { font-size: 10px; color: var(--fg-dim); padding: 4px 6px 2px; border-top: 1px solid var(--divider); margin-top: 4px; }

	/* M21 W4 behaviour 2 — cold-cache amber control (clickable to refresh, not a verdict pill). */
	.cold-cache {
		font-family: var(--font-ui); font-size: 10.5px; cursor: pointer;
		background: transparent; border: none; padding: 0;
		color: oklch(0.75 0.12 60);
	}
	.cold-cache:hover:not(:disabled) { color: oklch(0.82 0.14 60); text-decoration: underline; }
	.cold-cache:disabled { opacity: 0.6; cursor: default; }


	/* Content-type badges + profile tags */
	.badge-row-sm { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
	.ct-badge {
		font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
		background: var(--bg-elev3); color: var(--fg-muted);
	}
	.peer-tags { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
	.peer-tag {
		font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
		color: var(--accent);
		background: color-mix(in oklch, var(--accent) 12%, transparent);
	}

	/* M15 W7: removed the dead .modal-* block (unreferenced — grep-confirmed). */

	/* Tag filter bar */
	.tag-filter-row { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 0; padding: 0 24px; }
	.filter-tag {
		padding: 3px 10px; font-size: 11px; font-weight: 500;
		border: 1px solid transparent; border-radius: 999px;
		background: transparent; color: var(--fg-muted); cursor: pointer;
		font-family: var(--font-ui);
	}
	.filter-tag:hover { color: var(--accent); }
	.filter-tag-active { background: var(--accent-soft); border-color: var(--accent); color: var(--accent); }

	/* Local tags on contact cards */
	.tag-row { display: flex; flex-wrap: wrap; gap: 4px; margin: 5px 0 2px; align-items: center; min-height: 22px; }
	.local-tag {
		display: inline-flex; align-items: center; gap: 3px;
		padding: 1px 6px 1px 8px; border-radius: 4px; font-size: 11px; font-weight: 500;
		background: var(--bg-elev2); color: var(--fg-muted);
	}
	.tag-x {
		background: none; border: none; cursor: pointer; color: var(--fg-dim);
		font-size: 13px; line-height: 1; padding: 0; display: flex; align-items: center;
	}
	.tag-x:hover { color: var(--fg); }
	.tag-add-btn {
		font-size: 11px; color: var(--fg-dim); background: transparent; border: 1px dashed var(--border);
		border-radius: 4px; padding: 1px 7px; cursor: pointer; font-family: var(--font-ui);
	}
	.tag-add-btn:hover { border-color: var(--accent); color: var(--accent); }
	/* QURATOR-52 §5 — on the .hb-input contract; only the active-edit overrides are local
	   (accent border to mark editing, smaller font/radius for the inline tag slot). */
	.tag-input {
		font-size: 11px; border-color: var(--accent);
		border-radius: 4px; padding: 1px 7px; min-width: 60px;
	}

	.contact-detail {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-top: none;
		border-radius: 0 0 10px 10px;
		padding: 4px 14px 12px 56px;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

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
	}
	.pill-online .pill-dot { background: var(--online); }
	.pill-offline {
		color: var(--fg-muted);
		background: color-mix(in oklch, var(--fg-muted) 12%, transparent);
	}

	/* Group row on contact cards */
	.group-row { display: flex; flex-wrap: wrap; gap: 4px; margin: 3px 0 2px; align-items: center; min-height: 20px; }
	.group-pill {
		display: inline-flex; align-items: center; gap: 4px;
		padding: 1px 8px; border-radius: 4px; font-size: 11px; font-weight: 500;
		background: color-mix(in oklch, var(--accent) 10%, transparent);
		color: var(--accent);
	}
	/* M21 W5b: the group's colour dot inside a chip/editor row. Absent for a group with no colour. */
	.group-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; display: inline-block; }
	/* M21 W5b: `+` beside the chip row opens the membership popover. Styled like .tag-add-btn so the
	   face's group affordance reads as the same kind of control. */
	.group-add-btn {
		font-size: 11px; line-height: 1; color: var(--fg-dim); background: transparent;
		border: 1px dashed var(--border); border-radius: 4px; padding: 0 5px; cursor: pointer;
		font-family: var(--font-ui); display: inline-flex; align-items: center;
	}
	.group-add-btn:hover { border-color: var(--accent); color: var(--accent); }
	/* M20 W5: inline multi-select group membership editor (replaces the single-select that dropped
	   every other membership). Checkboxes over all groups + an Apply button, matching the inline-edit
	   pattern the tag row already uses in this detail area. */
	.group-edit-list { display: flex; flex-wrap: wrap; gap: 4px 10px; align-items: center; }
	.group-check {
		display: inline-flex; align-items: center; gap: 4px;
		font-size: 11.5px; color: var(--fg-muted); cursor: pointer;
	}
	.group-check input { margin: 0; cursor: pointer; }
	.group-edit-empty { font-size: 11px; color: var(--fg-dim); }

	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */

	/* M22 W3 — drag-to-group gesture visuals.
	   Lift: the source dims in place (opacity ~0.35) and stays put.
	   Aim: only the row under the cursor lights (accent border + soft fill) + outcome label;
	   other rows recede.
	   M22 W5 — .contact-selected is DISTINCT from hover and drag-source: a soft accent fill +
	   left border marks the selection persistently (not just while the cursor is over the row). */
	.contact-card.contact-selected {
		background: color-mix(in oklch, var(--accent) 10%, var(--bg-elev1));
		border-color: var(--accent);
		box-shadow: inset 0 0 0 2px var(--accent);
	}
	.contact-card.drag-source { opacity: 0.35; }
	.contact-card.drag-target {
		border-color: var(--accent);
		background: color-mix(in oklch, var(--accent) 8%, var(--bg-elev1));
	}
	.contact-card.drag-recede { opacity: 0.5; }
	.drag-outcome {
		font-size: 11px; font-weight: 600; color: var(--accent);
		padding: 2px 0 4px;
	}

	/* M22 W3 — naming popover (OverflowMenu shell). Two overlapping avatars + focused input. */
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
	/* M22 W5 — the count badge replaces the two avatars when a multi-selection is dropped. Same
	   footprint as the avatar pair (44×30), centred, accent-coloured. */
	.dg-count {
		width: 44px; height: 30px; border-radius: 15px;
		display: flex; align-items: center; justify-content: center;
		font-size: 13px; font-weight: 700; color: white;
		background: var(--accent);
		flex-shrink: 0;
	}
	/* QURATOR-52 §5 — on the .hb-input contract; only the layout (flex/padding) is local. */
	.dg-input { flex: 1; padding: 4px 8px; min-width: 0; }
	.dg-suggestions { display: flex; flex-wrap: wrap; gap: 5px; padding: 0 8px 4px; }
	.dg-chip {
		font-size: 11px; padding: 2px 8px; border-radius: 999px;
		background: var(--bg-elev3); border: 1px solid transparent; color: var(--fg-muted);
		cursor: pointer; font-family: var(--font-ui);
	}
	.dg-chip:hover { border-color: var(--accent); color: var(--accent); }
	.dg-footer { display: flex; justify-content: flex-end; gap: 6px; padding: 6px 8px 2px; border-top: 1px solid var(--divider); margin-top: 4px; }

	/* M22 W4 — drop-onto-existing-group affordances.
	   Active: accent border + soft fill, applied to a section heading or chip that accepts the drop.
	   Refused: muted strike-through styling; the cursor already shows 'none' from the dragover.
	   The .drop-hint states the outcome in words (move/add/already in X) so a plain drop reads as a
	   relocate, not a mystery. */
	.section-header.group-drop-active {
		color: var(--accent);
		background: color-mix(in oklch, var(--accent) 6%, var(--bg));
	}
	.section-header.group-drop-refused { opacity: 0.5; }
	.group-pill.group-drop-active {
		border: 1px solid var(--accent);
		background: color-mix(in oklch, var(--accent) 14%, transparent);
	}
	.group-pill.group-drop-refused { opacity: 0.45; }
	.drop-hint {
		margin-left: 6px; font-size: 10px; font-weight: 500; color: var(--accent);
		text-transform: none; letter-spacing: 0;
	}
	.section-header.group-drop-refused .drop-hint { color: var(--fg-dim); }

	/* M22 W7 — screen-reader-only live region (the refuse-state mirror). */
	.sr-only {
		position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
		overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0;
	}

	/* M22 W7 — respect prefers-reduced-motion: disable the drag/chevron transitions. */
	@media (prefers-reduced-motion: reduce) {
		.chevron, .tagfilter-chevron { transition: none; }
	}
</style>
