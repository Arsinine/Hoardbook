// M21 W5b — the Browse People panel gains group sections + the petname filter fix, guarded at the
// source. Source-scan idiom (see browse-w10, ask-access-w2, contacts-w1/w2/w4/w5): the route's
// onMount fan-out + $app/navigation goto + Tauri dialog make a full mount heavier than the wiring
// checks warrant, so we pin the things only the page can get wrong. The pure grouping/filtering
// helpers (groupByGroups, matchesQuery) already have DOM-free unit tests in
// lib/contacts-view.test.ts — this file pins that the route USES them.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const browseSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('M21 W5b — Browse People panel is grouped by the user\'s groups', () => {
	it('imports groupByGroups + matchesQuery from $lib/contacts-view (no new grouping logic)', () => {
		const s = browseSrc();
		expect(s).toMatch(/import \{ groupByGroups, matchesQuery \} from '\$lib\/contacts-view\.js'/);
	});

	it('groups are loaded via groupsGet and held in a groups state variable', () => {
		const s = browseSrc();
		expect(s).toMatch(/groupsGet/);
		expect(s).toMatch(/let groups: Group\[\] = \$state\(\[\]\)/);
	});

	it('the filtered list is fed to groupByGroups (sections, not a flat list)', () => {
		const s = browseSrc();
		// filteredContacts is the filter output; peopleSections is the grouped shape. Both derived.
		expect(s).toMatch(/let filteredContacts = \$derived\(\$contacts\.filter\(p => matchesQuery\(p, search\)\)\)/);
		expect(s).toMatch(/let peopleSections = \$derived\(groupByGroups\(filteredContacts, groups\)\)/);
	});

	it('the People panel renders peopleSections, not filteredContacts', () => {
		const s = browseSrc();
		expect(s).toMatch(/{#each peopleSections as section \(section\.key\)}/);
		// The old flat {#each filteredContacts as peer} is gone from the panel.
		const panelStart = s.indexOf('class="contact-list"');
		const panelEnd = s.indexOf('<!-- Right: browser');
		const panel = s.slice(panelStart, panelEnd);
		expect(panel).not.toMatch(/\{#each filteredContacts as peer/);
	});

	it('each section head carries a colour dot (when the group has one) + name + member count', () => {
		const s = browseSrc();
		expect(s).toMatch(/class="people-section-head"/);
		expect(s).toMatch(/class="people-group-dot"/);
		expect(s).toMatch(/background:\$\{secGroup\.color\}/);
		expect(s).toMatch(/class="people-section-title"/);
		expect(s).toMatch(/class="people-section-count"/);
		expect(s).toMatch(/\{section\.peers\.length\}/);
	});

	it('the dot is gated on the group having a colour (no broken dot for a no-colour group)', () => {
		const s = browseSrc();
		expect(s).toMatch(/\{#if secGroup\?\.color\}/);
	});

	it('the Ungrouped section key is a real section, not a hidden bucket', () => {
		// groupByGroups emits section.key === 'ungrouped' for peers in no group; the page must not
		// special-case it away. The secGroup lookup treats 'ungrouped' as the no-group section (null).
		const s = browseSrc();
		expect(s).toMatch(/section\.key === 'ungrouped' \? null : groups\.find/);
	});
});

describe('M21 W5b — Browse filter matches petname (mirrors Contacts)', () => {
	it('the filter uses matchesQuery, which matches display_name + petname + npub + bio + tags + …', () => {
		// The old inline filter only matched display_name + npub. The fix routes through matchesQuery,
		// the same helper Contacts uses, so petname (and every other field) is covered by construction.
		const s = browseSrc();
		expect(s).toMatch(/matchesQuery\(p, search\)/);
		// The old inline two-field filter is gone — no hand-rolled display_name + npub haystack.
		expect(s).not.toMatch(/display_name\?\.toLowerCase\(\)\.includes\(q\)/);
	});

	it('no hand-rolled filter haystack survives (the whole filter is matchesQuery)', () => {
		const s = browseSrc();
		// The old `const q = search.toLowerCase()` ladder is gone entirely.
		expect(s).not.toMatch(/const q = search\.toLowerCase\(\)/);
	});
});
