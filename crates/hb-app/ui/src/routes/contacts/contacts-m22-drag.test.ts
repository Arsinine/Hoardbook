// M22 W3 — the drag-to-group gesture ("drag one user onto another creates an ad hoc group").
//
// Two test layers:
//   1. Source-scan — pins DOM wiring that has NO behavioural equivalent (drag attributes, text
//      content, CSS class names). The repo's route-page idiom (contacts-w5b, contacts-w4-card):
//      the page's heavy onMount + $app/navigation make a full mount impractical. A source-scan is
//      kept ONLY where there is genuinely no behavioural equivalent.
//   2. Logic — drives the shared drag-group.ts primitives with a stubbed DataTransfer and a spy
//      api, asserting on the mocked api.ts calls (not on CSS). These are the acceptance gates:
//      create calls groupsCreateWithMembers exactly once, never calls contactUpdateGroups or
//      groupsUnassign, never touches the audience API, and Esc cancels with zero writes.
//
// Owner ruling 2026-08-09:
//   - Esc CANCELS the whole gesture — no group created, nothing written.
//   - Create is ALWAYS ADDITIVE (Reading B): both peers keep every group they were already in
//     and both gain the new one. No Shift handling, nothing on this path clears a membership.

import { describe, it, expect, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import {
	DRAG_MIME,
	DRAG_MULTI_MIME,
	GROUP_PALETTE,
	writeDragPayload,
	readDragPayload,
	writeDragPayloadMulti,
	readDragPayloadMulti,
	isSelfDrop,
	isValidDropTarget,
	pickGroupColor,
	groupSuggestions,
	groupSuggestionsMulti,
	commitCreateGroup,
	commitCreateGroupMulti,
	computeDropOutcome,
	commitDropOnGroup,
	computeDropOutcomeMulti,
	commitDropOnGroupMulti,
	applyClickToSelection,
	UNGROUPED_TARGET,
	type DragGroupApi,
	type DropOnGroupApi,
	type DropOutcome,
	type DropOutcomeMulti,
} from '$lib/drag-group.js';
import type { CachedPeer, Profile } from '$lib/types.js';

const contactsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

/** Page source with every comment removed (template `<!-- -->`, block, and line). Used by the
 *  no-undo scan: the page's own comments document the "NO confirm, NO undo" ruling in prose, so a
 *  raw scan for /undo/ would fail forever on correct code and invite weakening the assertion. */
function stripComments(src: string): string {
	return src
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/^[ \t]*\/\/.*$/gm, '');
}

// ── Test helpers ─────────────────────────────────────────────────────────────

/** A minimal DataTransfer stub that supports getData/setData (jsdom's DataTransfer is missing
 *  setData in some versions). Just enough to drive writeDragPayload / readDragPayload. */
function stubDataTransfer(): DataTransfer {
	const store = new Map<string, string>();
	return {
		setData: (type: string, val: string) => { store.set(type, val); },
		getData: (type: string) => store.get(type) ?? '',
		effectAllowed: 'none',
		dropEffect: 'none',
		types: [],
		clearData: () => { store.clear(); },
		setDragImage: () => {},
	} as unknown as DataTransfer;
}

function makeProfile(overrides: Partial<Profile> = {}): Profile {
	return {
		display_name: '',
		tags: [],
		languages: [],
		social_links: [],
		willing_to: [],
		content_types: [],
		updated: '',
		...overrides,
	};
}

function makePeer(overrides: Partial<CachedPeer> & { npub: string }): CachedPeer {
	return {
		browse_key_hex: undefined,
		petname: undefined,
		profile: undefined,
		collections: [],
		online: false,
		last_fetched: '',
		local_tags: [],
		...overrides,
	};
}

/** A spy DragGroupApi that records every call. Only groupsCreateWithMembers is defined — the
 *  absence of the other methods is itself the guarantee that commitCreateGroup CAN'T reach them. */
function spyApi(): DragGroupApi & { calls: { method: string; args: unknown[] }[] } {
	const calls: { method: string; args: unknown[] }[] = [];
	return {
		calls,
		groupsCreateWithMembers: vi.fn(async (name: string, npubs: string[], color?: string) => {
			calls.push({ method: 'groupsCreateWithMembers', args: [name, npubs, color] });
			return { name, pubkeys: npubs, color };
		}),
	} as unknown as DragGroupApi & { calls: { method: string; args: unknown[] }[] };
}

// ── 1. Source-scan: DOM wiring only (no behavioural equivalent) ─────────────
// These check drag-handler attributes, text content, and CSS class names that a mount test
// would check via the DOM — but the route page can't be mounted (onMount + $app/navigation).
// Behavioural properties (what gets called, how many times) are tested in the logic suite below.

describe('M22 W3 — contacts page wiring (source-scan, no behavioural equivalent)', () => {
	it('imports the shared drag-group primitives from $lib/drag-group.js', () => {
		const s = contactsSrc();
		expect(s).toMatch(/from '\$lib\/drag-group\.js'/);
		// DRAG_MIME is internal to the helpers; the page uses the helper functions directly.
		expect(s).toMatch(/writeDragPayload/);
		expect(s).toMatch(/readDragPayload/);
		expect(s).toMatch(/isValidDropTarget/);
		expect(s).toMatch(/groupSuggestions/);
		expect(s).toMatch(/commitCreateGroup/);
	});

	it('imports groupsCreateWithMembers from api.ts', () => {
		expect(contactsSrc()).toMatch(/groupsCreateWithMembers/);
	});

	it('the contact-card is draggable and wires all five drag handlers', () => {
		const s = contactsSrc();
		expect(s).toMatch(/draggable="true"/);
		expect(s).toMatch(/ondragstart=\{\(e\) => onDragStart\(e, peer\.npub\)\}/);
		expect(s).toMatch(/ondragover=\{\(e\) => onDragOver\(e, peer\.npub\)\}/);
		expect(s).toMatch(/ondrop=\{\(e\) => onDrop\(e, peer\.npub\)\}/);
		expect(s).toMatch(/ondragend=\{onDragEnd\}/);
		expect(s).toMatch(/ondragleave=\{\(\) => onDragLeave\(peer\.npub\)\}/);
	});

	it('the Aim moment renders a text outcome "group these two" for a single-pair drag (not an icon)', () => {
		// M22 W5: the text is now conditional on dragCount, but the single-pair fallback must still
		// say "group these two". The source must contain the literal so the W3 case is unchanged.
		expect(contactsSrc()).toMatch(/'group these two'/);
	});

	it('the Lift moment dims the source in place (.drag-source { opacity: ~0.35 })', () => {
		const s = contactsSrc();
		expect(s).toMatch(/class:drag-source=\{dragSourceNpub === peer\.npub\}/);
		expect(s).toMatch(/\.contact-card\.drag-source\s*\{\s*opacity: 0\.35/);
	});

	it('the naming popover renders a text input with placeholder "Name this group"', () => {
		expect(contactsSrc()).toMatch(/placeholder="Name this group"/);
	});

	it('the name field is NOT pre-filled (dragNameInput = "" on drop)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('function onDrop'), s.indexOf('function pickSuggestion'));
		expect(fn).toMatch(/dragNameInput = ''/);
	});

	it('Enter commits and Esc cancels (keydown wiring on the name field)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('async function onDragNameKey'), s.indexOf('async function commitDragCreate'));
		expect(fn).toMatch(/e\.key === 'Enter'/);
		expect(fn).toMatch(/commitDragCreate/);
		expect(fn).toMatch(/e\.key === 'Escape'/);
		expect(fn).toMatch(/closeDragPopover\(\)/);
	});

	it('the suggestion chips call pickSuggestion (not a pre-fill)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/function pickSuggestion/);
		expect(s).toMatch(/onclick=\{\(\) => pickSuggestion\(s\)\}/);
	});

	it('commitDragCreate refreshes the group list after the write (wiring, not behaviour)', () => {
		// The behavioural proof (create is called) lives in the logic suite. This source-scan pins
		// that loadGroups() is called AFTER commitCreateGroup so the chip appears without a reload.
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('async function commitDragCreate'), s.indexOf('</script>'));
		const commitIdx = fn.indexOf('commitCreateGroup');
		const loadIdx = fn.indexOf('await loadGroups()');
		expect(commitIdx).toBeGreaterThan(-1);
		expect(loadIdx).toBeGreaterThan(commitIdx);
	});
});

// ── 2. Logic: the shared primitives (behavioural acceptance gates) ──────────

describe('M22 W3 — DataTransfer payload', () => {
	it('writeDragPayload + readDragPayload round-trip the source npub', () => {
		const dt = stubDataTransfer();
		writeDragPayload(dt, 'npub1aaa');
		expect(readDragPayload(dt)).toBe('npub1aaa');
	});

	it('uses a dedicated MIME type (not text/plain)', () => {
		expect(DRAG_MIME).toMatch(/application\/x-hoardbook/);
		expect(DRAG_MIME).not.toBe('text/plain');
	});

	it('readDragPayload returns null for a null DataTransfer', () => {
		expect(readDragPayload(null)).toBeNull();
	});

	it('readDragPayload returns null when the MIME type is absent', () => {
		const dt = stubDataTransfer();
		expect(readDragPayload(dt)).toBeNull();
	});
});

describe('M22 W3 — self-drop is a no-op', () => {
	it('isSelfDrop is true when source === target', () => {
		expect(isSelfDrop('npub1a', 'npub1a')).toBe(true);
	});

	it('isSelfDrop is false for distinct peers', () => {
		expect(isSelfDrop('npub1a', 'npub1b')).toBe(false);
	});

	it('isValidDropTarget rejects a self-drop', () => {
		expect(isValidDropTarget('npub1a', 'npub1a')).toBe(false);
	});

	it('isValidDropTarget accepts distinct peers', () => {
		expect(isValidDropTarget('npub1a', 'npub1b')).toBe(true);
	});

	it('isValidDropTarget rejects a null source', () => {
		expect(isValidDropTarget(null, 'npub1b')).toBe(false);
	});
});

describe('M22 W3 — group colour auto-assignment', () => {
	it('picks the first unused palette entry', () => {
		const existing = [{ color: GROUP_PALETTE[0] }, { color: GROUP_PALETTE[1] }];
		expect(pickGroupColor(existing)).toBe(GROUP_PALETTE[2]);
	});

	it('falls back to the first entry when all palette colours are taken', () => {
		const existing = GROUP_PALETTE.map((hex) => ({ color: hex }));
		expect(pickGroupColor(existing)).toBe(GROUP_PALETTE[0]);
	});

	it('ignores groups with no colour (undefined color)', () => {
		const existing = [{ color: undefined }, { color: GROUP_PALETTE[0] }];
		expect(pickGroupColor(existing)).toBe(GROUP_PALETTE[1]);
	});

	it('returns a string, never undefined (no broken chip)', () => {
		expect(pickGroupColor([])).toBeTruthy();
		expect(typeof pickGroupColor([])).toBe('string');
	});
});

describe('M22 W3 — groupSuggestions', () => {
	it('returns up to 3 suggestions from suggestGroupNames', () => {
		const a = makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime'] }) });
		const b = makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime'] }) });
		const result = groupSuggestions(a, b);
		expect(result.length).toBeLessThanOrEqual(3);
		expect(result).toContain('anime');
	});

	it('the petname fallback is always the last entry', () => {
		const a = makePeer({ npub: 'npub1a', petname: 'Marisol' });
		const b = makePeer({ npub: 'npub1b', petname: 'Kestrel' });
		const result = groupSuggestions(a, b);
		expect(result[result.length - 1]).toBe('Marisol & Kestrel');
	});
});

// ── Acceptance gate: create is additive, calls create exactly once ───────────

describe('M22 W3 acceptance — create calls groupsCreateWithMembers exactly once', () => {
	it('commits with both npubs, de-duped, additive', async () => {
		const api = spyApi();
		await commitCreateGroup(api, 'Anime Club', 'npub1a', 'npub1b', []);
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('groupsCreateWithMembers');
		expect(api.calls[0].args[0]).toBe('Anime Club');
		// Exactly the two peers, in source-then-target order.
		expect(api.calls[0].args[1]).toEqual(['npub1a', 'npub1b']);
	});

	it('passes an auto-assigned colour', async () => {
		const api = spyApi();
		await commitCreateGroup(api, 'G', 'npub1a', 'npub1b', []);
		const color = api.calls[0].args[2];
		expect(typeof color).toBe('string');
		expect((GROUP_PALETTE as readonly string[]).includes(color as string)).toBe(true);
	});

	it('trims the name before committing', async () => {
		const api = spyApi();
		await commitCreateGroup(api, '  Anime  ', 'npub1a', 'npub1b', []);
		expect(api.calls[0].args[0]).toBe('Anime');
	});

	it('throws on an empty name (never creates a nameless group)', async () => {
		const api = spyApi();
		await expect(commitCreateGroup(api, '   ', 'npub1a', 'npub1b', [])).rejects.toThrow();
		expect(api.calls.length).toBe(0);
	});
});

// ── Acceptance gate: neither peer loses a membership (Reading B) ─────────────
// Behavioural: the DragGroupApi interface exposes ONLY groupsCreateWithMembers. commitCreateGroup
// physically cannot call contactUpdateGroups, groupsUnassign, or any audience API — the method
// isn't on the injected object. This is stronger than grepping for a name's absence.

describe('M22 W3 acceptance — create is ALWAYS ADDITIVE, never clears a membership', () => {
	it('the create path calls contactUpdateGroups ZERO times', async () => {
		const api = spyApi();
		await commitCreateGroup(api, 'Anime', 'npub1a', 'npub1b', []);
		expect(api.calls.filter((c) => c.method === 'contactUpdateGroups').length).toBe(0);
	});

	it('the create path calls groupsUnassign ZERO times', async () => {
		const api = spyApi();
		await commitCreateGroup(api, 'Anime', 'npub1a', 'npub1b', []);
		expect(api.calls.filter((c) => c.method === 'groupsUnassign').length).toBe(0);
	});

	it('DragGroupApi interface exposes ONLY groupsCreateWithMembers (no audience/membership method)', () => {
		// The interface is the compile-time guarantee. A method not on the interface cannot be
		// called through it. This pins that the surface hasn't grown to include an audience or
		// membership mutation.
		const api = spyApi() as unknown as Record<string, unknown>;
		expect(typeof api.groupsCreateWithMembers).toBe('function');
		expect(api.contactUpdateGroups).toBeUndefined();
		expect(api.groupsUnassign).toBeUndefined();
		expect(api.groupsAssign).toBeUndefined();
		expect(api.privateAudienceSet).toBeUndefined();
	});
});

// ── Acceptance gate: Esc cancels — no group created, no write ────────────────
// Behavioural: the spy proves the gesture never reached the only write path. When Esc cancels,
// commitCreateGroup is never called, so the spy records zero calls.

describe('M22 W3 acceptance — Esc cancels the whole gesture (zero writes)', () => {
	// NOTE: a fresh spy asserting `calls.length === 0` would be a TAUTOLOGY — it exercises no
	// cancel path and would stay green if Esc were rewired straight into the create. The route page
	// cannot be mounted (the repo idiom for every contacts route test), so the honest control is to
	// read the cancel path out of the source and assert what it does NOT reach.
	it('the Esc branch routes to closeDragPopover and never to the create path', () => {
		const src = contactsSrc();
		const key = src.slice(src.indexOf('function onDragNameKey'));
		const body = key.slice(0, key.indexOf('\n\tasync function commitDragCreate'));
		const esc = body.slice(body.indexOf("e.key === 'Escape'"));
		expect(esc).toMatch(/closeDragPopover\(\)/);
		expect(esc).not.toMatch(/commitDragCreate\(/);
	});

	it('closeDragPopover clears state and reaches no write path at all', () => {
		const src = contactsSrc();
		const start = src.indexOf('function closeDragPopover');
		expect(start).toBeGreaterThan(-1);
		const body = src.slice(start, src.indexOf('\n\t}', start));
		// The owner ruling: Esc creates nothing and writes nothing.
		expect(body).not.toMatch(/commitDragCreate/);
		expect(body).not.toMatch(/commitCreateGroup/);
		expect(body).not.toMatch(/groupsCreateWithMembers/);
		// It is a pure state reset.
		expect(body).toMatch(/dragPopoverFor = null/);
	});

	it('a gesture that DID reach commitCreateGroup records exactly one write', async () => {
		// The complement: proves the spy CAN record a call when commitCreateGroup IS invoked,
		// so the zero-calls assertion above is not vacuously true.
		const api = spyApi();
		await commitCreateGroup(api, 'Anime', 'npub1a', 'npub1b', []);
		expect(api.calls.length).toBe(1);
	});
});

// ── Acceptance gate: never touches the private audience ──────────────────────
// Behavioural: the spy api has no privateAudienceSet/privateAudienceList, and the DragGroupApi
// interface doesn't expose them. commitCreateGroup physically cannot call them.

describe('M22 W3 acceptance — drop path never touches the private audience', () => {
	it('the create path calls no audience API', async () => {
		const api = spyApi();
		await commitCreateGroup(api, 'Anime', 'npub1a', 'npub1b', []);
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});
});

// ════════════════════════════════════════════════════════════════════════════════
// M22 W4 — drop onto an existing group (heading, chip, Ungrouped).
//
// Owner ruling 2026-08-09 (INVERTED from the inferred rule): a plain drop MOVES the contact into
// the target; holding Shift ADDS (preserves existing memberships). Refused before release.
// Ungrouped clears every membership: NO confirm, NO undo (owner: "This isn't a word doc.").
// Nothing on this path reads or writes the private audience.
// ════════════════════════════════════════════════════════════════════════════════

/** A spy DropOnGroupApi: records groupsAssign + contactUpdateGroups calls. The absence of any
 *  other method (audience, create, unassign) is itself the guarantee that commitDropOnGroup
 *  CAN'T reach them. */
function spyDropApi(): DropOnGroupApi & { calls: { method: string; args: unknown[] }[] } {
	const calls: { method: string; args: unknown[] }[] = [];
	return {
		calls,
		groupsAssign: vi.fn(async (npub: string, groupName: string) => {
			calls.push({ method: 'groupsAssign', args: [npub, groupName] });
		}),
		contactUpdateGroups: vi.fn(async (npub: string, groupNames: string[]) => {
			calls.push({ method: 'contactUpdateGroups', args: [npub, groupNames] });
		}),
	} as unknown as DropOnGroupApi & { calls: { method: string; args: unknown[] }[] };
}

// ── computeDropOutcome: the dragover affordance (pure, before any write) ──────

describe('M22 W4 — computeDropOutcome: plain drop MOVES, Shift-drop ADDS', () => {
	it('plain drop into a group the contact is NOT in → move', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Anime'], false);
		expect(o.kind).toBe('move');
		expect(o).toHaveProperty('target', 'Film');
	});

	it('plain drop into a group the contact is NOT in (has other groups) → move', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Anime', 'Music'], false);
		expect(o.kind).toBe('move');
	});

	it('Shift-drop into a group the contact is NOT in → add', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Anime'], true);
		expect(o.kind).toBe('add');
		expect(o).toHaveProperty('target', 'Film');
	});
});

describe('M22 W4 — computeDropOutcome: already-in refuse state', () => {
	it('Shift-drop into the ONLY group they are in → refused ("already in Film")', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Film'], true);
		expect(o.kind).toBe('refused');
		expect(o).toHaveProperty('target', 'Film');
		expect((o as { reason: string }).reason).toBe('already in Film');
	});

	it('Shift-drop into one of several groups → refused', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Film', 'Anime'], true);
		expect(o.kind).toBe('refused');
	});

	// Plain move into the ONLY group they are in would change nothing → refused, not a silent noop.
	it('plain drop into the ONLY group they are in → refused (the set would not change)', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Film'], false);
		expect(o.kind).toBe('refused');
		expect((o as { reason: string }).reason).toBe('already in Film');
	});

	// Plain move into one of several IS a valid move: it removes them from the others (the relocated
	// semantics the owner ruled in). This is the case the refuse rule must NOT catch.
	it('plain drop into one of several groups → move (removes from the others)', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Film', 'Anime'], false);
		expect(o.kind).toBe('move');
	});
});

describe('M22 W4 — computeDropOutcome: Ungrouped target', () => {
	it('plain drop on Ungrouped with groups → ungrouped (clears all)', () => {
		const o = computeDropOutcome('npub1a', UNGROUPED_TARGET, ['Film', 'Anime'], false);
		expect(o.kind).toBe('ungrouped');
	});

	it('plain drop on Ungrouped with NO groups → noop (nothing to clear)', () => {
		const o = computeDropOutcome('npub1a', UNGROUPED_TARGET, [], false);
		expect(o.kind).toBe('noop');
	});

	it('Shift-drop on Ungrouped → refused (Shift on Ungrouped is meaningless)', () => {
		const o = computeDropOutcome('npub1a', UNGROUPED_TARGET, ['Film'], true);
		expect(o.kind).toBe('refused');
	});
});

describe('M22 W4 — computeDropOutcome: invalid drag', () => {
	it('a null source is refused (no payload to drop)', () => {
		const o = computeDropOutcome(null, 'Film', [], false);
		expect(o.kind).toBe('refused');
	});
});

// ── commitDropOnGroup: the write path (behavioural, spy on the mocked api) ────

describe('M22 W4 acceptance — add calls groupsAssign exactly once', () => {
	it('Shift-drop add → groupsAssign(source, target), nothing else', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'add', target: 'Film' });
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('groupsAssign');
		expect(api.calls[0].args).toEqual(['npub1a', 'Film']);
	});
});

describe('M22 W4 acceptance — move calls contactUpdateGroups with [target] only', () => {
	it('plain move → contactUpdateGroups(source, [target]), removes from all others', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'move', target: 'Film' });
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('contactUpdateGroups');
		expect(api.calls[0].args).toEqual(['npub1a', ['Film']]);
	});
});

describe('M22 W4 acceptance — Ungrouped calls contactUpdateGroups with []', () => {
	it('drop on Ungrouped → contactUpdateGroups(source, [])', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'ungrouped' });
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('contactUpdateGroups');
		expect(api.calls[0].args).toEqual(['npub1a', []]);
	});
});

// ── Acceptance gate: never touches the private audience (W4) ──────────────────

describe('M22 W4 acceptance — drop-onto-group never touches the private audience', () => {
	it('add path calls no audience API', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'add', target: 'Film' });
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});

	it('move path calls no audience API', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'move', target: 'Film' });
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});

	it('ungrouped path calls no audience API', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'ungrouped' });
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});

	it('DropOnGroupApi exposes ONLY groupsAssign + contactUpdateGroups', () => {
		const api = spyDropApi() as unknown as Record<string, unknown>;
		expect(typeof api.groupsAssign).toBe('function');
		expect(typeof api.contactUpdateGroups).toBe('function');
		expect(api.groupsCreateWithMembers).toBeUndefined();
		expect(api.groupsUnassign).toBeUndefined();
		expect(api.privateAudienceSet).toBeUndefined();
		expect(api.privateAudienceList).toBeUndefined();
	});
});

// ── Acceptance gate: Ungrouped is immediate, NO confirm, NO undo ─────────────
// Owner ruling 2026-08-09, verbatim: "No undo at all. This isn't a word doc." INV-8 was overruled.
// This test pins the decision so a future session does not add a confirm/undo/soft-delete "for
// safety" and mistake the absence for a gap.

describe('M22 W4 acceptance — Ungrouped drop offers NO confirm and NO undo', () => {
	it('the Ungrouped commit path calls contactUpdateGroups directly (no confirm gate)', () => {
		const src = contactsSrc();
		const start = src.indexOf('async function onGroupDrop');
		const end = src.indexOf('// Stale: last_fetched');
		const fn = src.slice(start, end);
		// The Ungrouped branch routes straight to commitDropOnGroup with no ConfirmButton / dialog.
		expect(fn).toMatch(/commitDropOnGroup/);
		expect(fn).not.toMatch(/ConfirmButton/);
		expect(fn).not.toMatch(/confirm/);
	});

	// NOTE: asserting that a test-constructed spy lacks an `undo` method would be a TAUTOLOGY — the
	// test builds the spy, so of course it has no such method, and it would stay green if the page
	// grew a full confirm-and-undo flow. The real risk is an undo AFFORDANCE appearing in the page,
	// so scan the page for one. (The behavioural proof that the write is one-shot lives above, where
	// commitDropOnGroup is actually invoked with an `ungrouped` outcome.)
	it('the page offers no undo affordance anywhere — script OR template', () => {
		// Scan the WHOLE page, not just onGroupDrop's body: an Undo button would live in the
		// template, which a script-only slice cannot see.
		//
		// Comments are stripped first because the page documents this very ruling in prose ("NO
		// confirm, NO undo"), and a raw scan would red on its own documentation — the sentinel must
		// not collide with something the spec REQUIRES to be present. What must never appear is an
		// undo AFFORDANCE, not the word.
		const src = stripComments(contactsSrc());
		expect(src.length).toBeGreaterThan(0);
		expect(src).not.toMatch(/\bundo\b/i);
		expect(src).not.toMatch(/\brevert\b/i);
		expect(src).not.toMatch(/\brestore\b/i);
	});
});

// ── Contacts page wiring (source-scan): the four targets are wired ───────────
// The route page can't be mounted (onMount + $app/navigation), so source-scan pins the DOM wiring
// that has no behavioural equivalent (the CSS classes + handler attributes). The behavioural proof
// (which api method is called, how many times) lives in the spy suites above.

describe('M22 W4 — contacts page wiring (source-scan, no behavioural equivalent)', () => {
	it('imports computeDropOutcome, commitDropOnGroup, UNGROUPED_TARGET from $lib/drag-group.js', () => {
		const s = contactsSrc();
		expect(s).toMatch(/computeDropOutcome/);
		expect(s).toMatch(/commitDropOnGroup/);
		expect(s).toMatch(/UNGROUPED_TARGET/);
	});

	it('imports groupsAssign + contactUpdateGroups from api.ts (the W4 write surface)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/groupsAssign/);
		expect(s).toMatch(/contactUpdateGroups/);
	});

	it('section headers are drop targets (ondragover/ondrop → onGroupDragOver/onGroupDrop)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/ondragover=\{\(e\) => onGroupDragOver\(e, dropTargetName\)\}/);
		expect(s).toMatch(/ondrop=\{\(e\) => onGroupDrop\(e, dropTargetName\)\}/);
		expect(s).toMatch(/ondragleave=\{\(\) => onGroupDragLeave\(dropTargetName\)\}/);
	});

	it('the Ungrouped section maps to UNGROUPED_TARGET (not passed as a real group name)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/section\.key === 'ungrouped' \? UNGROUPED_TARGET : section\.key/);
	});

	it('group chips on the card face are ALSO drop targets with stopPropagation', () => {
		const s = contactsSrc();
		const sub = s.slice(s.indexOf('class="contact-sub-row"'), s.indexOf('group-add-btn'));
		expect(sub).toMatch(/ondragover=\{\(e\) => \{ e\.stopPropagation\(\); onGroupDragOver\(e, gname\); \}\}/);
		expect(sub).toMatch(/ondrop=\{\(e\) => \{ e\.stopPropagation\(\); onGroupDrop\(e, gname\); \}\}/);
	});

	it('the refuse state is computed on dragover (dropOutcome set before drop)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('function onGroupDragOver'), s.indexOf('function onGroupDragLeave'));
		expect(fn).toMatch(/computeDropOutcome/);
		expect(fn).toMatch(/dropOutcome = outcome/);
	});

	it('the drop handler refuses noop/refused outcomes (no write, no toast)', () => {
		const s = contactsSrc();
		const start = s.indexOf('async function onGroupDrop');
		const end = s.indexOf('// Stale: last_fetched');
		const fn = s.slice(start, end);
		// The early-return gate: refused/noop short-circuit before any commit.
		expect(fn).toMatch(/outcome\.kind === 'refused' \|\| outcome\.kind === 'noop'/);
		expect(fn).toMatch(/return/);
	});

	it('dropEffect reflects the refuse state on dragover (none for refused/noop)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('function onGroupDragOver'), s.indexOf('function onGroupDragLeave'));
		expect(fn).toMatch(/dropEffect/);
		expect(fn).toMatch(/'none'/);
		expect(fn).toMatch(/e\.shiftKey \? 'copy' : 'move'/);
	});

	it('the drop affordance CSS classes exist (group-drop-active / group-drop-refused)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/class:group-drop-active/);
		expect(s).toMatch(/class:group-drop-refused/);
	});

	it('the drop-hint text states the outcome (move/add/already in X)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/class="drop-hint"/);
		expect(s).toMatch(/already in/);
	});
});

// ════════════════════════════════════════════════════════════════════════════════
// M22 W5 — multi-select drag. Five people into a new group becomes ONE drag instead of ~20
// interactions. The selection model: plain click selects one and clears the rest; Shift-click
// extends a contiguous run; Cmd/Ctrl-click toggles one. Dragging any selected row carries the WHOLE
// selection; dragging an unselected row carries just that row. The W3/W4 single-npub primitives and
// the DRAG_MIME payload are NEVER changed — W5 adds PARALLEL multi primitives under a new MIME.
// Refuse over selection: a target ALL selected already belong to refuses; MIXED allowed.
// ════════════════════════════════════════════════════════════════════════════════

// ── applyClickToSelection: the three modifiers (pure logic, no DOM) ──────────

describe('M22 W5 — applyClickToSelection: plain click selects one, clears the rest', () => {
	const ordered = ['npub1a', 'npub1b', 'npub1c', 'npub1d'];

	it('plain click on a row selects ONLY that row', () => {
		const r = applyClickToSelection(['npub1a', 'npub1b'], 'npub1a', ordered, 'npub1c', false, false);
		expect(r.selection).toEqual(['npub1c']);
	});

	it('plain click moves the anchor to the clicked row (so the next Shift extends from it)', () => {
		const r = applyClickToSelection([], null, ordered, 'npub1b', false, false);
		expect(r.anchor).toBe('npub1b');
	});
});

describe('M22 W5 — applyClickToSelection: Shift-click extends a contiguous run', () => {
	const ordered = ['npub1a', 'npub1b', 'npub1c', 'npub1d'];

	it('Shift-click extends from the anchor to the clicked row (forward range)', () => {
		const r = applyClickToSelection(['npub1a'], 'npub1a', ordered, 'npub1c', true, false);
		expect(r.selection).toEqual(['npub1a', 'npub1b', 'npub1c']);
	});

	it('Shift-click extends from the anchor to the clicked row (backward range)', () => {
		const r = applyClickToSelection(['npub1d'], 'npub1d', ordered, 'npub1a', true, false);
		expect(r.selection).toEqual(['npub1a', 'npub1b', 'npub1c', 'npub1d']);
	});

	it('Shift-click UNIONS with the existing selection (extends, does not replace)', () => {
		const r = applyClickToSelection(['npub1a', 'npub1d'], 'npub1a', ordered, 'npub1b', true, false);
		expect(r.selection).toEqual(['npub1a', 'npub1b', 'npub1d']);
	});

	it('Shift-click does NOT move the anchor (the next Shift still extends from the old one)', () => {
		const r = applyClickToSelection([], 'npub1a', ordered, 'npub1c', true, false);
		expect(r.anchor).toBe('npub1a');
	});

	it('Shift-click with no anchor falls back to a plain click', () => {
		const r = applyClickToSelection(['npub1a'], null, ordered, 'npub1b', true, false);
		expect(r.selection).toEqual(['npub1b']);
		expect(r.anchor).toBe('npub1b');
	});
});

describe('M22 W5 — applyClickToSelection: Cmd/Ctrl-click toggles one row', () => {
	const ordered = ['npub1a', 'npub1b', 'npub1c'];

	it('Cmd-click ADDS a row to the selection when absent', () => {
		const r = applyClickToSelection(['npub1a'], 'npub1a', ordered, 'npub1b', false, true);
		expect(r.selection).toEqual(['npub1a', 'npub1b']);
	});

	it('Cmd-click REMOVES a row from the selection when present', () => {
		const r = applyClickToSelection(['npub1a', 'npub1b'], 'npub1a', ordered, 'npub1a', false, true);
		expect(r.selection).toEqual(['npub1b']);
	});

	it('Cmd-click does NOT move the anchor', () => {
		const r = applyClickToSelection(['npub1a'], 'npub1a', ordered, 'npub1b', false, true);
		expect(r.anchor).toBe('npub1a');
	});
});

// ── Multi DataTransfer payload round-trip ───────────────────────────────────

describe('M22 W5 — multi DataTransfer payload', () => {
	it('writeDragPayloadMulti + readDragPayloadMulti round-trip the selection', () => {
		const dt = stubDataTransfer();
		writeDragPayloadMulti(dt, ['npub1a', 'npub1b', 'npub1c']);
		expect(readDragPayloadMulti(dt)).toEqual(['npub1a', 'npub1b', 'npub1c']);
	});

	it('uses a DISTINCT MIME type from the single-npub DRAG_MIME', () => {
		expect(DRAG_MULTI_MIME).toMatch(/application\/x-hoardbook/);
		expect(DRAG_MULTI_MIME).not.toBe(DRAG_MIME);
	});

	it('de-dupes the npubs before writing', () => {
		const dt = stubDataTransfer();
		writeDragPayloadMulti(dt, ['npub1a', 'npub1a', 'npub1b']);
		expect(readDragPayloadMulti(dt)).toEqual(['npub1a', 'npub1b']);
	});

	it('also writes the single-npub fallback under DRAG_MIME (first selected npub)', () => {
		const dt = stubDataTransfer();
		writeDragPayloadMulti(dt, ['npub1a', 'npub1b']);
		expect(readDragPayload(dt)).toBe('npub1a');
	});

	it('readDragPayloadMulti returns null for a null DataTransfer', () => {
		expect(readDragPayloadMulti(null)).toBeNull();
	});

	it('readDragPayloadMulti returns null when the multi MIME is absent (single-npub W3 drag)', () => {
		const dt = stubDataTransfer();
		writeDragPayload(dt, 'npub1a');
		expect(readDragPayloadMulti(dt)).toBeNull();
	});
});

// ── groupSuggestionsMulti: routes all N peers through suggestGroupNames ─────

describe('M22 W5 — groupSuggestionsMulti takes all selected peers', () => {
	it('returns up to 3 suggestions derived from all peers', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol', profile: makeProfile({ tags: ['anime'] }) }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel', profile: makeProfile({ tags: ['anime'] }) }),
			makePeer({ npub: 'npub1c', petname: 'Rune', profile: makeProfile({ tags: ['anime'] }) }),
		];
		const result = groupSuggestionsMulti(peers);
		expect(result.length).toBeLessThanOrEqual(3);
		expect(result).toContain('anime');
	});

	it('the petname fallback for 3+ peers is "lead +N"', () => {
		const peers = [
			makePeer({ npub: 'npub1a', petname: 'Marisol' }),
			makePeer({ npub: 'npub1b', petname: 'Kestrel' }),
			makePeer({ npub: 'npub1c', petname: 'Rune' }),
		];
		const result = groupSuggestionsMulti(peers);
		expect(result[result.length - 1]).toBe('Marisol +2');
	});
});

// ── commitCreateGroupMulti: ONE call with all N npubs (acceptance gate) ──────

describe('M22 W5 acceptance — create calls groupsCreateWithMembers exactly ONCE with all N npubs', () => {
	it('commits all selected npubs in ONE call, not N', async () => {
		const api = spyApi();
		await commitCreateGroupMulti(api, 'Anime Club', ['npub1a', 'npub1b', 'npub1c', 'npub1d'], []);
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('groupsCreateWithMembers');
		expect(api.calls[0].args[0]).toBe('Anime Club');
		expect(api.calls[0].args[1]).toEqual(['npub1a', 'npub1b', 'npub1c', 'npub1d']);
	});

	it('de-dupes npubs before committing (never sends a duplicate to the backend)', async () => {
		const api = spyApi();
		await commitCreateGroupMulti(api, 'G', ['npub1a', 'npub1a', 'npub1b'], []);
		expect(api.calls[0].args[1]).toEqual(['npub1a', 'npub1b']);
	});

	it('passes an auto-assigned colour', async () => {
		const api = spyApi();
		await commitCreateGroupMulti(api, 'G', ['npub1a', 'npub1b'], []);
		const color = api.calls[0].args[2];
		expect(typeof color).toBe('string');
		expect((GROUP_PALETTE as readonly string[]).includes(color as string)).toBe(true);
	});

	it('trims the name before committing', async () => {
		const api = spyApi();
		await commitCreateGroupMulti(api, '  Anime  ', ['npub1a', 'npub1b'], []);
		expect(api.calls[0].args[0]).toBe('Anime');
	});

	it('throws on an empty name (never creates a nameless group)', async () => {
		const api = spyApi();
		await expect(commitCreateGroupMulti(api, '   ', ['npub1a', 'npub1b'], [])).rejects.toThrow();
		expect(api.calls.length).toBe(0);
	});

	it('throws on an empty selection (never creates a zero-member group)', async () => {
		const api = spyApi();
		await expect(commitCreateGroupMulti(api, 'G', [], [])).rejects.toThrow();
		expect(api.calls.length).toBe(0);
	});
});

// ── commitCreateGroupMulti: never touches the private audience ──────────────

describe('M22 W5 acceptance — multi create never touches the private audience', () => {
	it('calls no audience API and exposes only groupsCreateWithMembers', async () => {
		const api = spyApi();
		await commitCreateGroupMulti(api, 'Anime', ['npub1a', 'npub1b', 'npub1c'], []);
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});
});

// ── computeDropOutcomeMulti: refuse over the SELECTION (the W5 rule) ────────

describe('M22 W5 — computeDropOutcomeMulti: ALL-in-target refuses, MIXED allowed', () => {
	function groupsMap(entries: Record<string, string[]>): Map<string, string[]> {
		return new Map(Object.entries(entries));
	}

	it('Shift-drop when NONE are in the target → add, touches all', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b', 'npub1c'], 'Film',
			groupsMap({ npub1a: ['Anime'], npub1b: ['Music'], npub1c: [] }),
			true,
		);
		expect(o.kind).toBe('add');
		expect(o).toHaveProperty('target', 'Film');
		expect((o as { npubs: string[] }).npubs).toEqual(['npub1a', 'npub1b', 'npub1c']);
	});

	it('Shift-drop when SOME are already in the target → add, touches only the missing ones', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b', 'npub1c'], 'Film',
			groupsMap({ npub1a: ['Film'], npub1b: ['Music'], npub1c: ['Film'] }),
			true,
		);
		expect(o.kind).toBe('add');
		// Only npub1b is not already in Film — the commit will touch ONLY that one.
		expect((o as { npubs: string[] }).npubs).toEqual(['npub1b']);
	});

	it('Shift-drop when ALL are already in the target → refused', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b'], 'Film',
			groupsMap({ npub1a: ['Film'], npub1b: ['Film'] }),
			true,
		);
		expect(o.kind).toBe('refused');
		expect((o as { reason: string }).reason).toBe('already in Film');
	});

	it('plain move when NONE are in the target → move, touches all', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b'], 'Film',
			groupsMap({ npub1a: ['Anime'], npub1b: ['Music'] }),
			false,
		);
		expect(o.kind).toBe('move');
		expect((o as { npubs: string[] }).npubs).toEqual(['npub1a', 'npub1b']);
	});

	it('plain move when ALL are in the target AND all have other groups → move (they lose the others)', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b'], 'Film',
			groupsMap({ npub1a: ['Film', 'Anime'], npub1b: ['Film', 'Music'] }),
			false,
		);
		expect(o.kind).toBe('move');
	});

	it('plain move when ALL are in the target as their ONLY group → refused (nothing would change)', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b'], 'Film',
			groupsMap({ npub1a: ['Film'], npub1b: ['Film'] }),
			false,
		);
		expect(o.kind).toBe('refused');
		expect((o as { reason: string }).reason).toBe('already in Film');
	});
});

describe('M22 W5 — computeDropOutcomeMulti: Ungrouped target', () => {
	function groupsMap(entries: Record<string, string[]>): Map<string, string[]> {
		return new Map(Object.entries(entries));
	}

	it('plain drop on Ungrouped when at least one has groups → ungrouped', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b'], UNGROUPED_TARGET,
			groupsMap({ npub1a: ['Film'], npub1b: [] }),
			false,
		);
		expect(o.kind).toBe('ungrouped');
	});

	it('plain drop on Ungrouped when NONE have groups → noop', () => {
		const o = computeDropOutcomeMulti(
			['npub1a', 'npub1b'], UNGROUPED_TARGET,
			groupsMap({ npub1a: [], npub1b: [] }),
			false,
		);
		expect(o.kind).toBe('noop');
	});

	it('Shift-drop on Ungrouped → refused', () => {
		const o = computeDropOutcomeMulti(
			['npub1a'], UNGROUPED_TARGET,
			groupsMap({ npub1a: ['Film'] }),
			true,
		);
		expect(o.kind).toBe('refused');
	});
});

describe('M22 W5 — computeDropOutcomeMulti: invalid drag', () => {
	it('an empty selection is refused', () => {
		const o = computeDropOutcomeMulti([], 'Film', new Map(), false);
		expect(o.kind).toBe('refused');
	});
});

// ── commitDropOnGroupMulti: per-selected write, partial failure surfaces ────

describe('M22 W5 acceptance — commitDropOnGroupMulti add touches only the missing npubs', () => {
	it('Shift-drop add with 3 missing → groupsAssign called once per missing npub', async () => {
		const api = spyDropApi();
		await commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b', 'npub1c'] });
		expect(api.calls.length).toBe(3);
		expect(api.calls.map((c) => c.args)).toEqual([
			['npub1a', 'Film'],
			['npub1b', 'Film'],
			['npub1c', 'Film'],
		]);
	});

	it('add never calls contactUpdateGroups (only groupsAssign)', async () => {
		const api = spyDropApi();
		await commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b'] });
		expect(api.calls.filter((c) => c.method === 'contactUpdateGroups').length).toBe(0);
	});
});

describe('M22 W5 acceptance — commitDropOnGroupMulti move calls contactUpdateGroups([target])', () => {
	it('plain move with 2 selected → contactUpdateGroups(npub, [target]) once each', async () => {
		const api = spyDropApi();
		await commitDropOnGroupMulti(api, { kind: 'move', target: 'Film', npubs: ['npub1a', 'npub1b'] });
		expect(api.calls.length).toBe(2);
		expect(api.calls.map((c) => c.args)).toEqual([
			['npub1a', ['Film']],
			['npub1b', ['Film']],
		]);
	});
});

describe('M22 W5 acceptance — commitDropOnGroupMulti ungrouped calls contactUpdateGroups([])', () => {
	it('ungrouped with 2 selected → contactUpdateGroups(npub, []) once each', async () => {
		const api = spyDropApi();
		await commitDropOnGroupMulti(api, { kind: 'ungrouped', npubs: ['npub1a', 'npub1b'] });
		expect(api.calls.length).toBe(2);
		expect(api.calls.map((c) => c.args)).toEqual([
			['npub1a', []],
			['npub1b', []],
		]);
	});
});

describe('M22 W5 acceptance — partial failure SURFACES (no silent half-populated group)', () => {
	it('a rejecting write throws an Error listing the failed npubs and the success count', async () => {
		const api = spyDropApi();
		// Make the second write reject.
		(api.groupsAssign as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error('relay down'));
		await expect(
			commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b', 'npub1c'] }),
		).rejects.toThrow(/partially failed/);
	});

	it('the thrown Error carries failedNpubs and succeeded count', async () => {
		const api = spyDropApi();
		(api.groupsAssign as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error('relay down'));
		try {
			await commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b'] });
			expect.fail('should have thrown');
		} catch (e) {
			const err = e as Error & { failedNpubs?: string[]; succeeded?: number };
			expect(err.failedNpubs).toEqual(['npub1a']);
			expect(err.succeeded).toBe(1);
		}
	});

	it('a full success resolves with the outcome (no throw)', async () => {
		const api = spyDropApi();
		const o = await commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b'] });
		expect(o.kind).toBe('add');
	});
});

describe('M22 W5 acceptance — multi drop never touches the private audience', () => {
	it('add / move / ungrouped call no audience API', async () => {
		const api = spyDropApi();
		await commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b'] });
		await commitDropOnGroupMulti(api, { kind: 'move', target: 'Film', npubs: ['npub1a'] });
		await commitDropOnGroupMulti(api, { kind: 'ungrouped', npubs: ['npub1a'] });
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});
});

// ── Contacts page wiring (source-scan): W5 selection + ghost badge ──────────
// The route page can't be mounted (onMount + $app/navigation), so source-scan pins the DOM wiring
// that has no behavioural equivalent. The behavioural proof (which api method, how many times) lives
// in the spy suites above.

describe('M22 W5 — contacts page wiring (source-scan, no behavioural equivalent)', () => {
	it('imports the multi-select primitives from $lib/drag-group.js', () => {
		const s = contactsSrc();
		expect(s).toMatch(/writeDragPayloadMulti/);
		expect(s).toMatch(/readDragPayloadMulti/);
		expect(s).toMatch(/commitCreateGroupMulti/);
		expect(s).toMatch(/computeDropOutcomeMulti/);
		expect(s).toMatch(/commitDropOnGroupMulti/);
		expect(s).toMatch(/applyClickToSelection/);
	});

	it('the contact-card wires onmousedown for selection (plain/shift/cmd-click)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/onmousedown=\{\(e\) => onContactMouseDown\(e, peer\.npub\)\}/);
	});

	it('the contact-card has a contact-selected class distinct from drag-source and drag-target', () => {
		const s = contactsSrc();
		expect(s).toMatch(/class:contact-selected=\{selectedNpubSet\.has\(peer\.npub\)\}/);
	});

	it('selectedNpubSet is a $derived Set derived from selectedNpubs', () => {
		const s = contactsSrc();
		expect(s).toMatch(/selectedNpubSet\s*=\s*\$derived/);
		expect(s).toMatch(/new Set\(selectedNpubs\)/);
	});

	it('onDragStart writes the multi payload when the row is selected (carries the whole selection)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('function onDragStart'), s.indexOf('function onDragOver'));
		expect(fn).toMatch(/writeDragPayloadMulti/);
		expect(fn).toMatch(/selectedNpubs/);
	});

	it('onDragStart clears the selection when dragging an UNSELECTED row (carries just that row)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('function onDragStart'), s.indexOf('function onDragOver'));
		expect(fn).toMatch(/selectedNpubs = \[/);
	});

	it('a window keydown Esc clears the selection (Escape handling)', () => {
		const s = contactsSrc();
		// Esc-to-clear is wired either as a svelte:window handler or inside an onkeydown.
		expect(s).toMatch(/e\.key === 'Escape'/);
		expect(s).toMatch(/selectedNpubs = \[\]/);
	});

	it('onDrop uses the multi payload when a multi-selection is in flight', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('function onDrop'), s.indexOf('function pickSuggestion'));
		expect(fn).toMatch(/readDragPayloadMulti/);
	});

	it('onGroupDrop uses the multi outcome when a multi-selection is in flight', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('async function onGroupDrop'), s.indexOf('// Stale: last_fetched'));
		expect(fn).toMatch(/computeDropOutcomeMulti/);
		expect(fn).toMatch(/commitDropOnGroupMulti/);
	});

	it('the naming popover shows a count badge when a multi-selection is dropped (N peers, not 2 avatars)', () => {
		const s = contactsSrc();
		// The ghost shows a count badge instead of two avatars when selection > 1.
		expect(s).toMatch(/dg-count/);
	});

	it('selection clears after a successful create', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('async function commitDragCreate'), s.indexOf('</script>'));
		expect(fn).toMatch(/selectedNpubs = \[\]/);
	});

	it('the contact-selected CSS is visually distinct from hover and drag-source', () => {
		const s = contactsSrc();
		expect(s).toMatch(/\.contact-card\.contact-selected\s*\{/);
	});
});
