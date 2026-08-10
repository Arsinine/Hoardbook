// M22 W4 — the remaining drop targets on the Browse People rail.
//
// Two test layers (same idiom as browse-w5b + contacts-m22-drag):
//   1. Source-scan — pins DOM wiring that has NO behavioural equivalent (the route can't be mounted
//      on @tauri-apps + $app/navigation). Kept ONLY where there is no behavioural equivalent.
//   2. Logic — drives the shared drag-group.ts primitives (computeDropOutcome, commitDropOnGroup)
//      with a spy api. These are the acceptance gates: plain drop MOVES (contactUpdateGroups),
//      Shift-drop ADDS (groupsAssign), refused before release, Ungrouped clears all, and the
//      audience API is never touched.
//
// Owner ruling 2026-08-09 (INVERTED): a plain drop MOVES; Shift makes it an ADD. The create gesture
// (W3) is always additive and untouched here. Ungrouped writes immediately and irreversibly: NO
// confirm, NO undo.

import { describe, it, expect, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import {
	computeDropOutcome,
	commitDropOnGroup,
	computeDropOutcomeMulti,
	commitDropOnGroupMulti,
	commitCreateGroupMulti,
	writeDragPayloadMulti,
	readDragPayloadMulti,
	computeDropInverse,
	computeDropInverseMulti,
	computeCreateInverse,
	commitInverse,
	commitInverseMulti,
	UNGROUPED_TARGET,
	type DropOnGroupApi,
	type DragGroupApi,
	type UndoApi,
	type DropOutcome,
	type DropInverse,
} from '$lib/drag-group.js';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

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

// ── Logic: one implementation shared with Contacts (behavioural acceptance gates) ───
// These prove the shared primitives behave correctly for the four targets. Contacts' suite has the
// exhaustive matrix; this file asserts the SAME primitives are what Browse's handlers route through,
// and that the W3 create gesture is untouched.

describe('M22 W4 logic — computeDropOutcome on the Browse targets', () => {
	it('plain drop into a group they are NOT in → move (relocates out of current groups)', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Anime'], false);
		expect(o.kind).toBe('move');
		expect(o).toHaveProperty('target', 'Film');
	});

	it('Shift-drop into a group they are NOT in → add (preserves existing memberships)', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Anime'], true);
		expect(o.kind).toBe('add');
		expect(o).toHaveProperty('target', 'Film');
	});

	it('Shift-drop into the ONLY group they are in → refused ("already in Film")', () => {
		const o = computeDropOutcome('npub1a', 'Film', ['Film'], true);
		expect(o.kind).toBe('refused');
		expect((o as { reason: string }).reason).toBe('already in Film');
	});

	it('plain drop on Ungrouped with groups → ungrouped', () => {
		expect(computeDropOutcome('npub1a', UNGROUPED_TARGET, ['Film'], false).kind).toBe('ungrouped');
	});

	it('plain drop on Ungrouped with NO groups → noop', () => {
		expect(computeDropOutcome('npub1a', UNGROUPED_TARGET, [], false).kind).toBe('noop');
	});

	it('Shift-drop on Ungrouped → refused', () => {
		expect(computeDropOutcome('npub1a', UNGROUPED_TARGET, ['Film'], true).kind).toBe('refused');
	});
});

describe('M22 W4 logic — commitDropOnGroup write paths (spy api)', () => {
	it('add → groupsAssign(source, target) exactly once', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'add', target: 'Film' });
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('groupsAssign');
		expect(api.calls[0].args).toEqual(['npub1a', 'Film']);
	});

	it('move → contactUpdateGroups(source, [target]) exactly once', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'move', target: 'Film' });
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('contactUpdateGroups');
		expect(api.calls[0].args).toEqual(['npub1a', ['Film']]);
	});

	it('ungrouped → contactUpdateGroups(source, []) exactly once', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'ungrouped' });
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].method).toBe('contactUpdateGroups');
		expect(api.calls[0].args).toEqual(['npub1a', []]);
	});

	it('never touches the private audience (add / move / ungrouped)', async () => {
		const api = spyDropApi();
		await commitDropOnGroup(api, 'npub1a', { kind: 'add', target: 'Film' });
		api.calls.length = 0;
		await commitDropOnGroup(api, 'npub1a', { kind: 'move', target: 'Film' });
		api.calls.length = 0;
		await commitDropOnGroup(api, 'npub1a', { kind: 'ungrouped' });
		const touched = api.calls.filter((c) =>
			c.method === 'privateAudienceSet' || c.method === 'privateAudienceList');
		expect(touched.length).toBe(0);
	});
});

// ── Browse page wiring (source-scan): the section-head drop target + W3 untouched ───

describe('M22 W4 — Browse People section heads are drop targets (source-scan)', () => {
	it('imports computeDropOutcome, commitDropOnGroup, UNGROUPED_TARGET from $lib/drag-group.js', () => {
		const s = browseSrc();
		expect(s).toMatch(/computeDropOutcome/);
		expect(s).toMatch(/commitDropOnGroup/);
		expect(s).toMatch(/UNGROUPED_TARGET/);
	});

	// Was: `expect(s).toMatch(/groupsAssign/)` over the WHOLE file — satisfied by the call site
	// itself, so it stayed green on Contacts while that page never imported groupsAssign at all
	// (a ReferenceError on every Shift-add, shipped in W4 400f850). Assert the import STATEMENT.
	// Mutation probe: dropping a symbol from the import line reds this.
	it('every api symbol the drop paths call is actually in the $lib/api.js import', () => {
		const s = browseSrc();
		const importLine = s.slice(s.indexOf('import {'), s.indexOf("from '$lib/api.js'"));
		for (const sym of ['groupsAssign', 'groupsUnassign', 'groupsDelete', 'contactUpdateGroups', 'groupsCreateWithMembers']) {
			expect(importLine).toContain(sym);
		}
	});

	it('the section head wires ondragover/ondragleave/ondrop → onGroupDragOver/Leave/Drop', () => {
		const s = browseSrc();
		const head = s.slice(s.indexOf('class="people-section-head"'), s.indexOf('people-group-dot'));
		expect(head).toMatch(/ondragover=\{\(e\) => onGroupDragOver\(e, dropTargetName\)\}/);
		expect(head).toMatch(/ondragleave=\{\(\) => onGroupDragLeave\(dropTargetName\)\}/);
		expect(head).toMatch(/ondrop=\{\(e\) => onGroupDrop\(e, dropTargetName\)\}/);
	});

	it('the Ungrouped section maps to UNGROUPED_TARGET (not passed as a real group name)', () => {
		expect(browseSrc()).toMatch(/section\.key === 'ungrouped' \? UNGROUPED_TARGET : section\.key/);
	});

	it('the refuse state is computed on dragover (dropEffect reflects it)', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('function onGroupDragOver'), s.indexOf('function onGroupDragLeave'));
		expect(fn).toMatch(/computeDropOutcome/);
		expect(fn).toMatch(/dropOutcome = outcome/);
		expect(fn).toMatch(/'none'/);
		expect(fn).toMatch(/e\.shiftKey \? 'copy' : 'move'/);
	});

	it('the drop handler refuses noop/refused outcomes (early return, no write)', () => {
		const s = browseSrc();
		const start = s.indexOf('async function onGroupDrop');
		const end = s.indexOf('</script>');
		const fn = s.slice(start, end);
		expect(fn).toMatch(/outcome\.kind === 'refused' \|\| outcome\.kind === 'noop'/);
		expect(fn).toMatch(/return/);
	});

	it('the drop affordance CSS classes exist (group-drop-active / group-drop-refused)', () => {
		const s = browseSrc();
		expect(s).toMatch(/class:group-drop-active/);
		expect(s).toMatch(/class:group-drop-refused/);
	});

	it('the drop-hint states the outcome in words (move / add / already in)', () => {
		const s = browseSrc();
		expect(s).toMatch(/class="drop-hint-browse"/);
	});

	it('onDragEnd clears the W4 affordance state alongside the W3 state', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('function onDragEnd'), s.indexOf('function onDrop'));
		expect(fn).toMatch(/dropOverTarget = null/);
		expect(fn).toMatch(/dropOutcome = null/);
	});

	// W6 acceptance: the toast must name the group AND the affected contact. Browse's twin of the
	// Contacts test — this is exactly the "one call site gets the fix, its twin doesn't" drift shape
	// CLAUDE.md §9 names. Mutation probe: reverting a label to `Added to ${...}` reds this.
	it('every single-drop toast label names the affected contact, not just the group', () => {
		const s = browseSrc();
		expect(s).toMatch(/Added \$\{dropName\(sourceNpub\)\} to \$\{committed\.target\}/);
		expect(s).toMatch(/Moved \$\{dropName\(sourceNpub\)\} to \$\{committed\.target\}/);
		expect(s).toMatch(/Moved \$\{dropName\(sourceNpub\)\} to Ungrouped/);
		expect(s).not.toMatch(/`(Added|Moved) to /);
	});
});

// ── W3 create gesture is untouched (regression guard) ─────────────────────────

describe('M22 W4 — W3 create gesture still wired on Browse (regression guard)', () => {
	it('the create-gesture imports (writeDragPayload, commitCreateGroup, …) survive', () => {
		const s = browseSrc();
		expect(s).toMatch(/writeDragPayload/);
		expect(s).toMatch(/readDragPayload/);
		expect(s).toMatch(/isValidDropTarget/);
		expect(s).toMatch(/isSelfDrop/);
		expect(s).toMatch(/groupSuggestions/);
		expect(s).toMatch(/commitCreateGroup/);
	});

	it('the People rows are still draggable + carry the W3 drop-to-create handlers', () => {
		const s = browseSrc();
		const row = s.slice(s.indexOf('class="contact-row"'), s.indexOf('avatar-wrap'));
		expect(row).toMatch(/draggable="true"/);
		expect(row).toMatch(/ondragstart=\{\(e\) => onDragStart\(e, peer\.npub\)\}/);
		expect(row).toMatch(/ondrop=\{\(e\) => onDrop\(e, peer\.npub\)\}/);
	});

	it('the W3 "group these two" outcome label survives', () => {
		expect(browseSrc()).toMatch(/group these two/);
	});
});

// ── Ungrouped: NO confirm, NO undo (pin the owner ruling) ────────────────────
// W6 adds undo to every drop kind EXCEPT Ungrouped. The old whole-page /undo/ scan is replaced
// with a behavioural test (computeDropInverse returns null for ungrouped) — STRICTLY STRONGER.

describe('M22 W4 — Browse Ungrouped drop: no confirm gate', () => {
	it('the Ungrouped commit path routes straight to commitDropOnGroup (no confirm gate)', () => {
		const src = browseSrc();
		const start = src.indexOf('async function onGroupDrop');
		const end = src.indexOf('</script>');
		const fn = src.slice(start, end);
		expect(fn).toMatch(/commitDropOnGroup/);
		expect(fn).not.toMatch(/ConfirmButton/);
		expect(fn).not.toMatch(/confirm/);
	});
});

describe('M22 W6 — Browse Ungrouped has NO inverse (behavioural)', () => {
	it('computeDropInverse returns null for an ungrouped outcome', () => {
		const inverse = computeDropInverse('npub1a', { kind: 'ungrouped' }, ['Film']);
		expect(inverse).toBeNull();
	});

	it('computeDropInverseMulti returns null for an ungrouped outcome (multi)', () => {
		const prior = new Map([['npub1a', ['Film']]]);
		const inverse = computeDropInverseMulti({ kind: 'ungrouped', npubs: ['npub1a'] }, prior);
		expect(inverse).toBeNull();
	});
});

// ════════════════════════════════════════════════════════════════════════════════
// M22 W5 — multi-select drag on the Browse People rail. Same shared primitives as Contacts (one
// implementation, two consumers). Contacts' suite has the exhaustive matrix; this file asserts the
// SAME multi primitives are what Browse's handlers route through, and that the W3/W4 paths survive.
// ════════════════════════════════════════════════════════════════════════════════

/** A spy DragGroupApi for the multi-create path (records groupsCreateWithMembers). */
function spyCreateApi(): DragGroupApi & { calls: { method: string; args: unknown[] }[] } {
	const calls: { method: string; args: unknown[] }[] = [];
	return {
		calls,
		groupsCreateWithMembers: vi.fn(async (name: string, npubs: string[], color?: string) => {
			calls.push({ method: 'groupsCreateWithMembers', args: [name, npubs, color] });
		}),
	} as unknown as DragGroupApi & { calls: { method: string; args: unknown[] }[] };
}

// ── W5 logic — the SAME multi primitives Browse routes through ──────────────

describe('M22 W5 logic — multi create calls groupsCreateWithMembers exactly once with all N', () => {
	it('commits all selected npubs in ONE call', async () => {
		const api = spyCreateApi();
		await commitCreateGroupMulti(api, 'Anime Club', ['npub1a', 'npub1b', 'npub1c'], []);
		expect(api.calls.length).toBe(1);
		expect(api.calls[0].args[1]).toEqual(['npub1a', 'npub1b', 'npub1c']);
	});
});

describe('M22 W5 logic — computeDropOutcomeMulti on Browse targets', () => {
	it('Shift-drop when SOME already in target → add, touches only the missing', () => {
		const groups = new Map([['npub1a', ['Film']], ['npub1b', ['Music']]]);
		const o = computeDropOutcomeMulti(['npub1a', 'npub1b'], 'Film', groups, true);
		expect(o.kind).toBe('add');
		expect((o as { npubs: string[] }).npubs).toEqual(['npub1b']);
	});

	it('Shift-drop when ALL already in target → refused', () => {
		const groups = new Map([['npub1a', ['Film']], ['npub1b', ['Film']]]);
		const o = computeDropOutcomeMulti(['npub1a', 'npub1b'], 'Film', groups, true);
		expect(o.kind).toBe('refused');
	});

	it('plain drop on Ungrouped when SOME have groups → ungrouped', () => {
		const groups = new Map([['npub1a', ['Film']], ['npub1b', []]]);
		const o = computeDropOutcomeMulti(['npub1a', 'npub1b'], UNGROUPED_TARGET, groups, false);
		expect(o.kind).toBe('ungrouped');
	});
});

describe('M22 W5 logic — commitDropOnGroupMulti add writes once per touched npub', () => {
	it('add with 2 missing → 2 groupsAssign calls', async () => {
		const api = spyDropApi();
		await commitDropOnGroupMulti(api, { kind: 'add', target: 'Film', npubs: ['npub1a', 'npub1b'] });
		expect(api.calls.length).toBe(2);
		expect(api.calls.every((c) => c.method === 'groupsAssign')).toBe(true);
	});
});

describe('M22 W5 logic — multi DataTransfer payload round-trip', () => {
	it('write + read round-trip the selection', () => {
		const store = new Map<string, string>();
		const dt = {
			setData: (t: string, v: string) => { store.set(t, v); },
			getData: (t: string) => store.get(t) ?? '',
			effectAllowed: 'none',
			dropEffect: 'none',
			types: [],
			clearData: () => { store.clear(); },
			setDragImage: () => {},
		} as unknown as DataTransfer;
		writeDragPayloadMulti(dt, ['npub1a', 'npub1b', 'npub1c']);
		expect(readDragPayloadMulti(dt)).toEqual(['npub1a', 'npub1b', 'npub1c']);
	});
});

// ── Browse page wiring (source-scan): W5 selection + ghost badge ────────────

describe('M22 W5 — Browse page wiring (source-scan, no behavioural equivalent)', () => {
	it('imports the multi-select primitives from $lib/drag-group.js', () => {
		const s = browseSrc();
		expect(s).toMatch(/writeDragPayloadMulti/);
		expect(s).toMatch(/readDragPayloadMulti/);
		expect(s).toMatch(/commitCreateGroupMulti/);
		expect(s).toMatch(/computeDropOutcomeMulti/);
		expect(s).toMatch(/commitDropOnGroupMulti/);
		expect(s).toMatch(/applyClickToSelection/);
	});

	it('the contact-row wires onmousedown for selection', () => {
		const s = browseSrc();
		expect(s).toMatch(/onmousedown=\{\(e\) => onPeerMouseDown\(e, peer\.npub\)\}/);
	});

	it('the contact-row has a contact-selected class distinct from drag-source/drag-target', () => {
		const s = browseSrc();
		expect(s).toMatch(/class:peer-selected=\{selectedNpubSet\.has\(peer\.npub\)\}/);
	});

	it('selectedNpubSet is a $derived Set derived from selectedNpubs', () => {
		const s = browseSrc();
		expect(s).toMatch(/selectedNpubSet\s*=\s*\$derived/);
	});

	it('onDragStart writes the multi payload when the row is selected', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('function onDragStart'), s.indexOf('function onDragOver'));
		expect(fn).toMatch(/writeDragPayloadMulti/);
		expect(fn).toMatch(/selectedNpubs/);
	});

	it('onDrop uses the multi payload when a multi-selection is in flight', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('function onDrop'), s.indexOf('function closeDragPopover'));
		expect(fn).toMatch(/readDragPayloadMulti/);
	});

	it('onGroupDrop uses the multi outcome when a multi-selection is in flight', () => {
		const s = browseSrc();
		const start = s.indexOf('async function onGroupDrop');
		const end = s.indexOf('</script>');
		const fn = s.slice(start, end);
		expect(fn).toMatch(/computeDropOutcomeMulti/);
		expect(fn).toMatch(/commitDropOnGroupMulti/);
	});

	it('the naming popover shows a count badge for a multi-selection', () => {
		const s = browseSrc();
		expect(s).toMatch(/dg-count/);
	});

	it('selection clears after a successful create', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('async function commitDragCreate'), s.indexOf('let dropOverTarget'));
		expect(fn).toMatch(/selectedNpubs = \[\]/);
	});

	it('Esc clears the selection', () => {
		const s = browseSrc();
		expect(s).toMatch(/e\.key === 'Escape'/);
		expect(s).toMatch(/selectedNpubs = \[\]/);
	});
});

// ════════════════════════════════════════════════════════════════════════════════
// M22 W6 — undo. Same shared primitives as Contacts (one implementation, two consumers). Browse's
// suite asserts the SAME inverse primitives are what Browse's handlers route through.
// ════════════════════════════════════════════════════════════════════════════════

/** A spy UndoApi: records groupsDelete + groupsUnassign + contactUpdateGroups. */
function spyUndoApi(): UndoApi & { calls: { method: string; args: unknown[] }[] } {
	const calls: { method: string; args: unknown[] }[] = [];
	return {
		calls,
		groupsDelete: vi.fn(async (name: string) => {
			calls.push({ method: 'groupsDelete', args: [name] });
		}),
		groupsUnassign: vi.fn(async (npub: string, groupName: string) => {
			calls.push({ method: 'groupsUnassign', args: [npub, groupName] });
		}),
		contactUpdateGroups: vi.fn(async (npub: string, groupNames: string[]) => {
			calls.push({ method: 'contactUpdateGroups', args: [npub, groupNames] });
		}),
	} as unknown as UndoApi & { calls: { method: string; args: unknown[] }[] };
}

describe('M22 W6 logic — the inverse of each drop kind (table-driven, shared primitives)', () => {
	const cases: {
		name: string;
		outcome: DropOutcome;
		priorGroups: string[];
		expectedKind: DropInverse['kind'] | null;
	}[] = [
		{ name: 'add',       outcome: { kind: 'add', target: 'Film' },          priorGroups: ['Anime'],      expectedKind: 'unassign' },
		{ name: 'move',      outcome: { kind: 'move', target: 'Film' },         priorGroups: ['Anime'],      expectedKind: 'restore-groups' },
		{ name: 'ungrouped', outcome: { kind: 'ungrouped' },                    priorGroups: ['Film'],       expectedKind: null },
		{ name: 'refused',   outcome: { kind: 'refused', target: 'Film', reason: 'x' }, priorGroups: ['Film'], expectedKind: null },
		{ name: 'noop',      outcome: { kind: 'noop' },                         priorGroups: [],             expectedKind: null },
	];

	for (const c of cases) {
		it(`${c.name} → ${c.expectedKind ?? 'no inverse'}`, () => {
			const inverse = computeDropInverse('npub1a', c.outcome, c.priorGroups);
			if (c.expectedKind === null) {
				expect(inverse).toBeNull();
			} else {
				expect((inverse as DropInverse).kind).toBe(c.expectedKind);
			}
		});
	}
});

describe('M22 W6 logic — commitInverse (shared primitives)', () => {
	it('add inverse → groupsUnassign', async () => {
		const api = spyUndoApi();
		await commitInverse(api, { kind: 'unassign', npub: 'npub1a', groupName: 'Film' });
		expect(api.calls[0].method).toBe('groupsUnassign');
	});

	it('move inverse → contactUpdateGroups(prior)', async () => {
		const api = spyUndoApi();
		await commitInverse(api, { kind: 'restore-groups', npub: 'npub1a', groupNames: ['Anime'] });
		expect(api.calls[0].method).toBe('contactUpdateGroups');
	});

	it('create inverse → groupsDelete', async () => {
		const api = spyUndoApi();
		await commitInverse(api, computeCreateInverse('Anime'));
		expect(api.calls[0].method).toBe('groupsDelete');
	});
});

// ── Browse page wiring (source-scan): W6 undo wiring ─────────────────────────

describe('M22 W6 — Browse page wiring (source-scan)', () => {
	it('imports the undo primitives from $lib/drag-group.js', () => {
		const s = browseSrc();
		expect(s).toMatch(/computeDropInverse/);
		expect(s).toMatch(/computeCreateInverse/);
		expect(s).toMatch(/commitInverse/);
	});

	it('onDragStart ASSIGNS priorGroupsByNpub at drag start (not just a mention)', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('function onDragStart'), s.indexOf('function onDragOver'));
		expect(fn).toMatch(/priorGroupsByNpub\s*=\s*/);
	});

	it('onGroupDrop reads priorGroupsByNpub for the move inverse', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('async function onGroupDrop'), s.indexOf('</script>'));
		expect(fn).toMatch(/priorGroupsByNpub/);
		expect(fn).toMatch(/computeDropInverse/);
	});

	it('commitDragCreate registers a create inverse', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('async function commitDragCreate'), s.indexOf('let dropOverTarget'));
		expect(fn).toMatch(/computeCreateInverse/);
		expect(fn).toMatch(/toastWithAction/);
	});
});
