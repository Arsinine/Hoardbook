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
