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
