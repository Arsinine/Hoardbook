// M21 W5b — group chips carry their colour + the `+` membership popover, guarded at the source.
// The repo's established route-page idiom is source scanning (see contacts-w1, contacts-w2,
// contacts-w4-card, contacts-w5, contacts-w5-dataloss, ask-access-w2, browse-w10): the route's
// onMount fan-out and $app/navigation goto make a full mount heavier than the wiring checks warrant.
//
// Two behaviours live here only because the route wires them:
//   1. Group chips render a colour dot from Group.color (absent ⇒ no dot, never a broken chip).
//   2. A `+` beside the chip row opens a popover whose Apply sends the FULL checked set to
//      contactUpdateGroups — the same full-set semantics the expanded editor uses. The single-select
//      defect (contacts-w5-dataloss) must not be reintroduced via the popover.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const contactsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('M21 W5b behaviour 1 — group chips carry their colour dot', () => {
	it('a groupColor helper looks up Group.color by name', () => {
		const s = contactsSrc();
		expect(s).toMatch(/function groupColor\(name: string\)/);
		// Reads the colour off the same `groups` array that seeds the editor — not re-derived.
		expect(s).toMatch(/groups\.find\(g => g\.name === name\)\?\.color/);
	});

	it('the face chip renders a dot when the group has a colour', () => {
		const s = contactsSrc();
		// Inside the contact-sub-row chip loop: a `.group-dot` whose background is the looked-up color.
		const subRowIdx = s.indexOf('class="contact-sub-row"');
		const detailIdx = s.indexOf('class="contact-detail"');
		const faceRegion = s.slice(subRowIdx, detailIdx);
		expect(faceRegion).toMatch(/groupColor\(gname\)/);
		expect(faceRegion).toMatch(/class="group-dot"/);
		expect(faceRegion).toMatch(/background:\$\{gcolor\}/);
	});

	it('the detail chip renders the same dot (face and detail agree)', () => {
		const s = contactsSrc();
		const detailStart = s.indexOf('class="contact-detail"');
		const detailEnd = s.indexOf('OverflowMenu', detailStart);
		const detail = s.slice(detailStart, detailEnd);
		// The non-editing chip branch inside the detail also renders the dot from groupColor.
		expect(detail).toMatch(/groupColor\(gname\)/);
		expect(detail).toMatch(/class="group-dot"/);
	});

	it('a group with no colour renders the chip with NO dot (guarded by {#if gcolor})', () => {
		// The colour is always read into a const and the dot is gated on it being present — so a
		// no-colour group renders the chip text with no dot, never a broken/invisible chip.
		const s = contactsSrc();
		expect(s).toMatch(/\{@const gcolor = groupColor\(gname\)\}/);
		expect(s).toMatch(/\{#if gcolor\}<span class="group-dot"/);
	});

	it('the dot has a dedicated style rule (not a re-used pill-dot)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/\.group-dot\s*\{/);
	});
});

describe('M21 W5b behaviour 2 — `+` opens a membership popover on the collapsed card', () => {
	it('the face renders a `+` control beside the chip row', () => {
		const s = contactsSrc();
		const subRowIdx = s.indexOf('class="contact-sub-row"');
		const detailIdx = s.indexOf('class="contact-detail"');
		const faceRegion = s.slice(subRowIdx, detailIdx);
		expect(faceRegion).toMatch(/class="group-add-btn"/);
		expect(faceRegion).toMatch(/>\+</);
	});

	it('the `+` opens the popover via openGroupPopover (anchored, aria-haspopup)', () => {
		const s = contactsSrc();
		expect(s).toMatch(/function openGroupPopover\(npub: string, anchor: HTMLElement\)/);
		expect(s).toMatch(/aria-haspopup="true"/);
		expect(s).toMatch(/aria-expanded=\{groupPopoverFor === peer\.npub\}/);
		expect(s).toMatch(/openGroupPopover\(peer\.npub, e\.currentTarget\)/);
	});

	it('the popover seeds its draft from CURRENT memberships (pre-check loses nothing)', () => {
		// The data-loss guard the expanded editor enforces — seed from contactGroups(npub), not empty.
		const s = contactsSrc();
		const fn = s.slice(
			s.indexOf('function openGroupPopover'),
			s.indexOf('function togglePopoverGroup'),
		);
		expect(fn).toMatch(/new Set\(contactGroups\(npub\)\)/);
	});

	it('Apply sends the WHOLE checked set to contactUpdateGroups, not a single name', () => {
		// The single-select defect (contacts-w5-dataloss) must not return via the popover. The apply
		// handler spreads the draft set — the same shape handleSaveGroups uses for the expanded editor.
		const s = contactsSrc();
		const fn = s.slice(
			s.indexOf('async function applyGroupPopover'),
			s.indexOf('// M20 W2:'),
		);
		expect(fn).toMatch(/\[\.\.\.\(groupPopoverDraft\[npub\] \?\? \[\]\)\]/);
		expect(fn).toMatch(/contactUpdateGroups\(npub, names\)/);
	});

	it('Cancel discards the draft (sets groupPopoverFor = null without calling contactUpdateGroups)', () => {
		const s = contactsSrc();
		// The Cancel button just closes — no save. The discard is the absence of a contactUpdateGroups
		// call in the cancel path, which is the popover's onclose handler.
		expect(s).toMatch(/onclose=\{\(\) => \(groupPopoverFor = null\)\}/);
	});

	it('the popover offers "+ New group…" routed to the existing CreateGroupDialog', () => {
		const s = contactsSrc();
		expect(s).toMatch(/\+ New group…/);
		expect(s).toMatch(/class="gp-new"[^>]*onclick=\{\(\) => \(createGroupOpen = true\)\}/);
	});

	it('the popover reuses the OverflowMenu shell (anchored, Escape-to-close), not a new surface', () => {
		// No new positioning code — the popover is a second OverflowMenu with membership contents.
		const s = contactsSrc();
		expect(s).toMatch(/OverflowMenu open=\{groupPopoverFor === peer\.npub\}/);
		expect(s).toMatch(/anchor=\{groupPopoverAnchor\}/);
	});

	it('the expanded checkbox editor still works (an additional route, not a replacement)', () => {
		// The existing beginGroupEdit/handleSaveGroups path is untouched.
		const s = contactsSrc();
		expect(s).toMatch(/function beginGroupEdit\(hb_id: string\)/);
		expect(s).toMatch(/async function handleSaveGroups\(hb_id: string\)/);
	});
});
