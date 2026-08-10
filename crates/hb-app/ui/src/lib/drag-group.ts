// Shared drag-to-group primitives (M22 W3). Pure logic — no Svelte, no DOM.
//
// The Contacts and Browse pages both wire these into HTML5 drag events. W4 (drop onto an
// existing group) will reuse the same DataTransfer helpers and palette.
//
// Owner ruling 2026-08-09: create is ALWAYS ADDITIVE (Reading B). On the drop-to-create
// gesture, both peers keep every group they were already in and both gain the new one. There
// is no Shift handling on the create gesture, and nothing on this path ever clears a membership.

import type { CachedPeer, Group } from './types.js';
import { suggestGroupNames } from './group-suggest.js';

/** The MIME type the source npub is carried under in the DataTransfer. */
export const DRAG_MIME = 'application/x-hoardbook-drag-npub';

/** The 8-hue group palette (matches CreateGroupDialog's swatch picker). Auto-colour picks from
 *  here. Duplicated rather than extracted so this module stays dependency-free; if either copy
 *  changes, the other should too. */
export const GROUP_PALETTE = [
	'#e05c5c', '#e08a3c', '#d6c23c', '#6cbf5e',
	'#3fb0a8', '#4d8fe0', '#7d6ce0', '#c15ec2',
] as const;

/** The minimal API surface commitCreateGroup needs. Passed in (not imported) so the test can
 *  inject a spy without module-level mocking, and so the function CAN'T call anything outside
 *  this interface — the strongest possible guarantee that it never touches the private audience
 *  or clears a membership. */
export interface DragGroupApi {
	groupsCreateWithMembers(name: string, npubs: string[], color?: string): Promise<unknown>;
}

/** Pick a colour for a new group from palette entries not already used by an existing group.
 *  Falls back to the first entry when all are taken (graceful — never throws, never returns
 *  undefined). */
export function pickGroupColor(existing: Pick<Group, 'color'>[]): string {
	const used = new Set(existing.map((g) => g.color).filter((c): c is string => !!c));
	const free = GROUP_PALETTE.find((hex) => !used.has(hex));
	return free ?? GROUP_PALETTE[0];
}

/** Write the source npub into the DataTransfer so the drop target can read it. Called on
 *  dragstart. */
export function writeDragPayload(dt: DataTransfer, npub: string): void {
	dt.setData(DRAG_MIME, npub);
	dt.effectAllowed = 'copy';
}

/** Read the source npub back from the DataTransfer, or null if absent (wrong drag type, or no
 *  DataTransfer). Called on drop. */
export function readDragPayload(dt: DataTransfer | null): string | null {
	if (!dt) return null;
	try {
		return dt.getData(DRAG_MIME) || null;
	} catch {
		return null;
	}
}

/** A self-drop (source onto itself) is a no-op, not a one-member group. */
export function isSelfDrop(sourceNpub: string | null, targetNpub: string): boolean {
	return sourceNpub === targetNpub;
}

/** A valid drop target is a different contact than the source (and the source exists). */
export function isValidDropTarget(sourceNpub: string | null, targetNpub: string): boolean {
	return sourceNpub !== null && !isSelfDrop(sourceNpub, targetNpub);
}

/** Suggestions for the naming popover — up to 3 from W2's suggestGroupNames. These are
 *  suggestions for the user to accept or discard, never a default — the caller must NOT
 *  pre-fill the name field from the return. */
export function groupSuggestions(source: CachedPeer, target: CachedPeer): string[] {
	return suggestGroupNames([source, target]);
}

/** Commit: create the group with both peers, additive (Reading B). Calls
 *  groupsCreateWithMembers exactly once with [sourceNpub, targetNpub]. Does NOT call
 *  contactUpdateGroups, groupsUnassign, or any audience API — both peers keep every group they
 *  were already in and gain this one. Returns the colour that was auto-assigned.
 *
 *  Esc cancels the whole gesture before this function is ever called — so the cancel path
 *  calls this zero times (pinned by the cancel test). */
export async function commitCreateGroup(
	api: DragGroupApi,
	name: string,
	sourceNpub: string,
	targetNpub: string,
	existingGroups: Pick<Group, 'color'>[],
): Promise<string> {
	const trimmed = name.trim();
	if (!trimmed) throw new Error('Group name is empty');
	const color = pickGroupColor(existingGroups);
	await api.groupsCreateWithMembers(trimmed, [sourceNpub, targetNpub], color);
	return color;
}

// ── M22 W4 — drop onto an existing group (heading, chip, or Ungrouped) ──────────────────────────
//
// The governing rule was INVERTED by the owner on 2026-08-09. A plain drop MOVES the contact into
// the target (relocates them out of their current groups); holding Shift makes it an ADD (preserves
// existing memberships). This applies ONLY to drops onto a group that already exists — W3's
// drop-to-create gesture is always additive and has no Shift handling.
//
// The four targets (identical in Contacts and Browse):
//   → a group heading       — plain move / Shift-add. Add = groupsAssign; move = contactUpdateGroups([target]).
//   → a group chip on a card — same outcome, nearer target.
//   → somewhere they already are — REFUSED before release ("already in Film"); no write, no toast.
//   → "Ungrouped"            — clears every membership via contactUpdateGroups([]). No confirm, no undo.

/** The sentinel target name that means "clear all memberships". This is NOT a real group name; it is
 *  recognised by computeDropOutcome + commitDropOnGroup, and never passed to the backend as a group. */
export const UNGROUPED_TARGET = '__ungrouped__';

/** The minimal API surface commitDropOnGroup needs. Passed in (not imported) so the test can inject
 *  a spy, and so the function CAN'T call anything outside this interface — the strongest possible
 *  guarantee that it never touches the private audience. groupsAssign (add-one) and
 *  contactUpdateGroups (full-set-replace) are the only two membership mutations on this path. */
export interface DropOnGroupApi {
	groupsAssign(npub: string, groupName: string): Promise<unknown>;
	contactUpdateGroups(npub: string, groupNames: string[]): Promise<unknown>;
}

/** The kind of drop the user is about to make, computed on dragover so the affordance can show it
 *  before the drop fires. `refused` carries a human reason ("already in Film"); `noop` means the
 *  drop would change nothing (e.g. Ungrouped when they have no groups) and must short-circuit with
 *  no write and no toast. */
export type DropOutcome =
	| { kind: 'move'; target: string }
	| { kind: 'add'; target: string }
	| { kind: 'ungrouped' }
	| { kind: 'refused'; target: string; reason: string }
	| { kind: 'noop' };

/** Compute the outcome of dropping sourceNpub onto targetGroupName (a real group name, or
 *  UNGROUPED_TARGET). Pure: no writes, no I/O. Used on dragover to light the affordance and on
 *  drop to decide what to commit.
 *
 *  - shiftKey + a real group they are NOT already in → add (groupsAssign).
 *  - shiftKey + a real group they ARE already in     → refused ("already in {target}").
 *  - plain + a real group                             → move (contactUpdateGroups([target])).
 *    A plain move INTO a group they are already in is a valid move ONLY if they are also in other
 *    groups (it removes them from the others); if it is their ONLY group, it is a noop (the set
 *    would not change), so it is refused as "already in {target}" to keep the affordance honest.
 *  - plain + UNGROUPED + they have groups             → ungrouped (contactUpdateGroups([])).
 *  - plain + UNGROUPED + they have no groups          → noop (nothing to clear).
 *  - shiftKey + UNGROUPED                              → refused (Shift on Ungrouped is meaningless).
 */
export function computeDropOutcome(
	sourceNpub: string | null,
	targetGroupName: string,
	sourceGroups: string[],
	shiftKey: boolean,
): DropOutcome {
	// A null source is an invalid drag (no payload) — refuse rather than write.
	if (sourceNpub === null) return { kind: 'refused', target: targetGroupName, reason: 'invalid drag' };
	// Ungrouped is special-cased before any membership reasoning.
	if (targetGroupName === UNGROUPED_TARGET) {
		if (shiftKey) return { kind: 'refused', target: targetGroupName, reason: 'Shift on Ungrouped is meaningless' };
		if (sourceGroups.length === 0) return { kind: 'noop' };
		return { kind: 'ungrouped' };
	}
	const already = sourceGroups.includes(targetGroupName);
	if (shiftKey) {
		// Shift-add into a group they are already in is a refused no-op (nothing would change).
		if (already) return { kind: 'refused', target: targetGroupName, reason: `already in ${targetGroupName}` };
		return { kind: 'add', target: targetGroupName };
	}
	// Plain move into a group they are already in: only meaningful if they have OTHER groups to lose.
	if (already && sourceGroups.length <= 1) return { kind: 'refused', target: targetGroupName, reason: `already in ${targetGroupName}` };
	return { kind: 'move', target: targetGroupName };
}

/** Commit the drop. Calls the backend exactly once: groupsAssign for an add,
 *  contactUpdateGroups for a move or ungrouped. Never calls the audience API. Returns the outcome
 *  that was committed (so the caller can toast the right verb), or throws if the backend rejects.
 *
 *  noop / refused outcomes call the backend ZERO times — the caller must gate the call on
 *  outcome.kind being one of the write kinds (this function enforces it too). */
export async function commitDropOnGroup(
	api: DropOnGroupApi,
	sourceNpub: string,
	outcome: Exclude<DropOutcome, { kind: 'refused' } | { kind: 'noop' }>,
): Promise<DropOutcome> {
	if (outcome.kind === 'add') {
		await api.groupsAssign(sourceNpub, outcome.target);
	} else if (outcome.kind === 'move') {
		await api.contactUpdateGroups(sourceNpub, [outcome.target]);
	} else {
		// ungrouped — clear every membership.
		await api.contactUpdateGroups(sourceNpub, []);
	}
	return outcome;
}

// ── M22 W5 — multi-select drag ──────────────────────────────────────────────────────────────────
//
// Five people into a new group becomes ONE drag instead of ~20 interactions. The selection model is
// the standard one: a plain click selects one and clears the rest; Shift-click extends a contiguous
// run; Cmd/Ctrl-click toggles a single row. Dragging any selected row carries the WHOLE selection;
// dragging an unselected row carries just that row and clears the selection first (matches every
// file manager and email client the user has used).
//
// Design rule: the single-npub W3/W4 primitives and the single-npub DRAG_MIME payload are NEVER
// changed — W3/W4 behaviour is pinned by existing tests and stays intact. W5 adds PARALLEL
// multi-select primitives under a NEW MIME type carrying a JSON array. The page wiring reads the
// multi payload first and falls back to the single payload so a W3 single-peer drag keeps working
// unchanged.
//
// The refuse rule is aggregated over the selection: a target ALL selected contacts already belong
// to refuses; a MIXED selection is allowed and adds/moves only the ones not already in the target.
// Partial failure on commit MUST surface — the page never silently leaves a group half-populated.

/** The MIME type the WHOLE selection (a JSON array of npubs) is carried under in the DataTransfer.
 *  Distinct from DRAG_MIME so the single-npub W3/W4 path is untouched. */
export const DRAG_MULTI_MIME = 'application/x-hoardbook-drag-npubs';

/** Apply a click to the selection model and return the new selection (as a new array — the caller
 *  reassigns). Pure: no DOM, no Svelte. Models the three standard modifiers:
 *  - plain click   → selects ONLY this row (clears the rest). This is the default and the most
 *                     common case; it is what happens when the user clicks without holding any key.
 *  - shift-click   → extends a contiguous run from the anchor (last plain-clicked row) to this row.
 *                     If no anchor is set yet, behaves like a plain click.
 *  - cmd/ctrl-click → toggles this single row in the selection (add if absent, remove if present)
 *                     WITHOUT moving the anchor, so the next Shift-click still extends from the old
 *                     anchor. This matches Finder/Explorer/Gmail semantics.
 *
 *  `orderedNpubs` is the rendered row order (used to compute the contiguous range for Shift). It
 *  MUST be the same order the user sees; the caller passes it in so this function stays pure. */
export function applyClickToSelection(
	current: string[],
	anchor: string | null,
	orderedNpubs: string[],
	clickedNpub: string,
	shiftKey: boolean,
	metaKey: boolean,
): { selection: string[]; anchor: string | null } {
	if (shiftKey && anchor !== null) {
		const start = orderedNpubs.indexOf(anchor);
		const end = orderedNpubs.indexOf(clickedNpub);
		if (start === -1 || end === -1) {
			// Anchor or clicked row not in the rendered set (stale anchor) — fall back to plain click.
			return { selection: [clickedNpub], anchor: clickedNpub };
		}
		const lo = Math.min(start, end);
		const hi = Math.max(start, end);
		const range = orderedNpubs.slice(lo, hi + 1);
		// Union with the existing selection: Shift-click ADDS the run to what is already selected
		// (so Shift-click into an existing selection extends it, matching file managers).
		const merged = new Set([...current, ...range]);
		// Preserve the rendered order in the result so the selection is deterministic.
		const selection = orderedNpubs.filter((n) => merged.has(n));
		return { selection, anchor };
	}
	if (metaKey) {
		const set = new Set(current);
		if (set.has(clickedNpub)) set.delete(clickedNpub);
		else set.add(clickedNpub);
		// Preserve rendered order; anchor is NOT moved (so the next Shift extends from the old one).
		const selection = orderedNpubs.filter((n) => set.has(n));
		return { selection, anchor };
	}
	// Plain click: select only this row, and it becomes the new anchor.
	return { selection: [clickedNpub], anchor: clickedNpub };
}

/** Write the WHOLE selection (a JSON array of npubs) into the DataTransfer. Called on dragstart
 *  when the dragged row is part of a multi-selection. De-dupes and preserves the order given. */
export function writeDragPayloadMulti(dt: DataTransfer, npubs: string[]): void {
	const unique = Array.from(new Set(npubs));
	dt.setData(DRAG_MULTI_MIME, JSON.stringify(unique));
	// Also write the single-npub payload under DRAG_MIME with the first selected npub, so a drop
	// target that only reads the single payload (W3 drop-to-create) still gets a usable source.
	// Drop targets that understand the multi payload read it instead and ignore the single fallback.
	dt.setData(DRAG_MIME, unique[0] ?? '');
	dt.effectAllowed = 'copy';
}

/** Read the whole selection back from the DataTransfer, or null if the multi payload is absent
 *  (a single-peer W3 drag, or a foreign drag type). Called on drop / dragover. Always returns a
 *  de-duped array when present; null means "no multi payload — fall back to single-npub read". */
export function readDragPayloadMulti(dt: DataTransfer | null): string[] | null {
	if (!dt) return null;
	try {
		const raw = dt.getData(DRAG_MULTI_MIME);
		if (!raw) return null;
		const parsed = JSON.parse(raw);
		if (!Array.isArray(parsed)) return null;
		const strs = parsed.filter((v): v is string => typeof v === 'string' && v.length > 0);
		if (strs.length === 0) return null;
		return Array.from(new Set(strs));
	} catch {
		return null;
	}
}

/** Suggestions for the naming popover when N peers are dropped together. Just routes W2's
 *  suggestGroupNames with the whole selection — it already takes an array, so multi-select needs no
 *  new logic here. Kept as a named export so the page wiring is symmetrical with the W3 single-pair
 *  groupSuggestions helper and so the test can pin the multi path independently. */
export function groupSuggestionsMulti(peers: CachedPeer[]): string[] {
	return suggestGroupNames(peers);
}

/** Commit: create the group with ALL selected peers, additive (Reading B). Calls
 *  groupsCreateWithMembers exactly ONCE with the whole selection — never N times. Does NOT call
 *  contactUpdateGroups, groupsUnassign, or any audience API. Returns the colour that was
 *  auto-assigned. The selection MUST be de-duped by the caller (writeDragPayloadMulti already
 *  de-dupes); this function de-dupes defensively one more time so a duplicate never reaches the
 *  backend. */
export async function commitCreateGroupMulti(
	api: DragGroupApi,
	name: string,
	npubs: string[],
	existingGroups: Pick<Group, 'color'>[],
): Promise<string> {
	const trimmed = name.trim();
	if (!trimmed) throw new Error('Group name is empty');
	const unique = Array.from(new Set(npubs));
	if (unique.length === 0) throw new Error('No contacts selected');
	const color = pickGroupColor(existingGroups);
	await api.groupsCreateWithMembers(trimmed, unique, color);
	return color;
}

/** The outcome of a multi-select drop onto an existing group, computed on dragover so the
 *  affordance can show it before the drop fires. The refuse rule is aggregated over the selection:
 *  - ALL selected already in target (and nothing would change) → refused.
 *  - MIXED selection (some in, some out) → allowed; the commit applies only to the ones not already
 *    in the target. This is the case the refuse rule must NOT catch.
 *  - NONE in target → plain add/move like the single-source case.
 *
 *  `groupsByNpub` maps each selected npub to the list of group names it currently belongs to. */
export type DropOutcomeMulti =
	| { kind: 'move'; target: string; npubs: string[] }
	| { kind: 'add'; target: string; npubs: string[] }
	| { kind: 'ungrouped'; npubs: string[] }
	| { kind: 'refused'; target: string; reason: string }
	| { kind: 'noop' };

/** Compute the outcome of dropping the whole selection onto targetGroupName (a real group name, or
 *  UNGROUPED_TARGET). Pure: no writes, no I/O. Mirrors computeDropOutcome's refuse semantics but
 *  aggregated across the selection — refused ONLY when EVERY selected contact is already in the
 *  target (so a mixed selection is allowed and the commit applies only to the missing ones). The
 *  `npubs` field on the write-kind outcomes carries ONLY the contacts the commit will actually
 *  touch (the ones not already in the target for add; the full selection for move/ungrouped). */
export function computeDropOutcomeMulti(
	selectedNpubs: string[],
	targetGroupName: string,
	groupsByNpub: Map<string, string[]>,
	shiftKey: boolean,
): DropOutcomeMulti {
	if (selectedNpubs.length === 0) {
		return { kind: 'refused', target: targetGroupName, reason: 'invalid drag' };
	}
	// Ungrouped is special-cased before any membership reasoning, same as the single-source path.
	if (targetGroupName === UNGROUPED_TARGET) {
		if (shiftKey) return { kind: 'refused', target: targetGroupName, reason: 'Shift on Ungrouped is meaningless' };
		// If EVERY selected contact already has no groups, the drop changes nothing → noop.
		const anyWithGroups = selectedNpubs.some((n) => (groupsByNpub.get(n) ?? []).length > 0);
		if (!anyWithGroups) return { kind: 'noop' };
		return { kind: 'ungrouped', npubs: selectedNpubs };
	}
	// Partition the selection into already-in-target and not-in-target.
	const alreadyIn = selectedNpubs.filter((n) => (groupsByNpub.get(n) ?? []).includes(targetGroupName));
	const notIn = selectedNpubs.filter((n) => !(groupsByNpub.get(n) ?? []).includes(targetGroupName));
	if (shiftKey) {
		// Shift-add: refused ONLY when EVERY selected contact is already in the target (nothing to add).
		if (notIn.length === 0) {
			return { kind: 'refused', target: targetGroupName, reason: `already in ${targetGroupName}` };
		}
		return { kind: 'add', target: targetGroupName, npubs: notIn };
	}
	// Plain move: refused ONLY when EVERY selected contact is in the target AND the target is their
	// ONLY group (the move would change nothing for any of them). A contact in the target who also
	// has other groups WOULD lose them under a plain move, so the drop is allowed when any selected
	// contact has other groups to lose OR any is not yet in the target.
	if (alreadyIn.length === selectedNpubs.length) {
		// Everyone is already in the target. Refuse only if NO one has other groups to lose.
		const anyWithOthers = selectedNpubs.some((n) => {
			const gs = groupsByNpub.get(n) ?? [];
			return gs.length > 1;
		});
		if (!anyWithOthers) {
			return { kind: 'refused', target: targetGroupName, reason: `already in ${targetGroupName}` };
		}
	}
	return { kind: 'move', target: targetGroupName, npubs: selectedNpubs };
}

/** Commit a multi-select drop onto an existing group. Applies the write per selected contact that
 *  the outcome actually touches (the `npubs` field), using the same single-write semantics as
 *  commitDropOnGroup: groupsAssign for add, contactUpdateGroups for move/ungrouped. Never calls the
 *  audience API.
 *
 *  PARTIAL FAILURE SURFACES: if any per-contact write rejects, the remaining writes still run (so
 *  the group is not left half-populated by a single bad row) and the function resolves by throwing
 *  an AggregateError-style Error carrying the npubs that failed and the count that succeeded. The
 *  caller MUST surface this — never silently leave a partial result. A full success resolves with
 *  the outcome (so the caller can toast the right verb). */
export async function commitDropOnGroupMulti(
	api: DropOnGroupApi,
	outcome: Exclude<DropOutcomeMulti, { kind: 'refused' } | { kind: 'noop' }>,
): Promise<Exclude<DropOutcomeMulti, { kind: 'refused' } | { kind: 'noop' }>> {
	const targets = outcome.npubs;
	// Build the per-npub write promise. Each touched contact makes exactly ONE backend call.
	const writes: Promise<{ npub: string; ok: true } | { npub: string; ok: false; err: unknown }>[] =
		targets.map(async (npub) => {
			try {
				if (outcome.kind === 'add') {
					await api.groupsAssign(npub, outcome.target);
				} else if (outcome.kind === 'move') {
					await api.contactUpdateGroups(npub, [outcome.target]);
				} else {
					await api.contactUpdateGroups(npub, []);
				}
				return { npub, ok: true as const };
			} catch (err) {
				return { npub, ok: false as const, err };
			}
		});
	const results = await Promise.all(writes);
	const failed = results.filter((r): r is { npub: string; ok: false; err: unknown } => !r.ok);
	if (failed.length > 0) {
		const failedNpubs = failed.map((r) => r.npub);
		const succeeded = results.length - failed.length;
		const err = new Error(
			`Drop partially failed: ${succeeded} succeeded, ${failed.length} failed: ${failedNpubs.join(', ')}`,
		);
		(err as Error & { failedNpubs: string[] }).failedNpubs = failedNpubs;
		(err as Error & { succeeded: number }).succeeded = succeeded;
		throw err;
	}
	return outcome;
}

// ── M22 W6 — undo: the inverse of each drop ───────────────────────────────────
//
// Owner ruling 2026-08-09 (verbatim): "No undo at all. This isn't a word doc." — that applies to the
// Ungrouped drop ONLY. Every other drop kind that has an inverse gets one. The inverse is computed
// PURELY from the outcome + the prior membership set, and executed through a SEPARATE narrow UndoApi
// (not DropOnGroupApi / DragGroupApi) so the invariant-guarding narrowness of those interfaces is
// preserved.
//
// Prior membership capture: the move inverse needs the group set the contact was in BEFORE the move.
// It MUST be captured at drag START (not drop), or a slow write between start and drop can race it —
// the contact's groups at drop time may already reflect an intermediate state. The pure functions
// below take priorGroups as a parameter so the page (which owns the capture point) cannot get it
// wrong by re-reading at drop time.

/** The minimal API surface the undo path needs. Deliberately SEPARATE from DropOnGroupApi and
 *  DragGroupApi so those invariant-guarding interfaces stay narrow — widening them to add
 *  groupsDelete/groupsUnassign would erode the guarantee that commitDropOnGroup / commitCreateGroup
 *  CAN'T reach a delete. Passed in (not imported) so the test injects a spy. Never includes the
 *  private audience API. */
export interface UndoApi {
	groupsDelete(name: string): Promise<unknown>;
	groupsUnassign(npub: string, groupName: string): Promise<unknown>;
	contactUpdateGroups(npub: string, groupNames: string[]): Promise<unknown>;
}

/** The inverse of a drop — a descriptor the toast holds so the Undo button can replay it. `null`
 *  means "no inverse" (ungrouped by owner ruling; refused / noop because nothing happened). */
export type DropInverse =
	| { kind: 'delete-group'; name: string }
	| { kind: 'unassign'; npub: string; groupName: string }
	| { kind: 'restore-groups'; npub: string; groupNames: string[] };

/** Compute the inverse of a single-source drop outcome. Pure: no writes, no I/O.
 *  - add (Shift-drop onto a group) → unassign the contact from that one group.
 *  - move (plain drop onto a group) → restore the contact to their PRIOR group set.
 *  - ungrouped → null (no inverse, by owner ruling — "This isn't a word doc.").
 *  - refused / noop → null (nothing happened).
 *
 *  `priorGroups` is the contact's membership set captured at drag START (not drop). */
export function computeDropInverse(
	sourceNpub: string,
	outcome: DropOutcome,
	priorGroups: string[],
): DropInverse | null {
	if (outcome.kind === 'add') return { kind: 'unassign', npub: sourceNpub, groupName: outcome.target };
	if (outcome.kind === 'move') return { kind: 'restore-groups', npub: sourceNpub, groupNames: priorGroups };
	// ungrouped: no inverse, by owner ruling. refused / noop: nothing happened.
	return null;
}

/** Compute the inverse of a multi-select drop. Returns one inverse per affected contact so each can
 *  be undone independently. For move/ungrouped-multi the inverse is per-npub restore-groups (with
 *  each contact's own prior set). Ungrouped returns null per the ruling — no entry at all. */
export function computeDropInverseMulti(
	outcome: Exclude<DropOutcomeMulti, { kind: 'refused' } | { kind: 'noop' }>,
	priorGroupsByNpub: Map<string, string[]>,
): DropInverse[] | null {
	if (outcome.kind === 'ungrouped') return null; // no inverse, by owner ruling.
	if (outcome.kind === 'add') {
		return outcome.npubs.map((npub) => ({ kind: 'unassign' as const, npub, groupName: outcome.target }));
	}
	// move: restore each touched contact to their prior group set.
	return outcome.npubs.map((npub) => ({
		kind: 'restore-groups' as const,
		npub,
		groupNames: priorGroupsByNpub.get(npub) ?? [],
	}));
}

/** Compute the inverse of a create-group gesture (W3 single-pair and W5 multi). The group was brand
 *  new, so the inverse is safe: delete it. */
export function computeCreateInverse(name: string): DropInverse {
	return { kind: 'delete-group', name };
}

/** Execute an inverse via the UndoApi. One backend call per inverse entry. The caller passes a
 *  spy UndoApi in tests. Never touches the private audience. */
export async function commitInverse(api: UndoApi, inverse: DropInverse): Promise<void> {
	if (inverse.kind === 'delete-group') {
		await api.groupsDelete(inverse.name);
	} else if (inverse.kind === 'unassign') {
		await api.groupsUnassign(inverse.npub, inverse.groupName);
	} else {
		await api.contactUpdateGroups(inverse.npub, inverse.groupNames);
	}
}

/** Execute N inverse entries (the multi-select undo). One backend call per entry. Partial failure
 *  surfaces (throws). Never touches the private audience. */
export async function commitInverseMulti(api: UndoApi, inverses: DropInverse[]): Promise<void> {
	for (const inv of inverses) {
		await commitInverse(api, inv);
	}
}
