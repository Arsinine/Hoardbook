// @vitest-environment jsdom
// M22 W8 — the ONE shared group-membership editor (Contacts' W5b popover extracted so Browse gains
// the same editor). Behavioural tests drive the component with real DOM; the wiring tests source-scan
// the two route pages to pin that BOTH consumers route Apply through contactUpdateGroups (the same
// full-set command) and that NEITHER touches the private audience.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import GroupMembershipPopover from './GroupMembershipPopover.svelte';
import type { Group } from '../types.js';

afterEach(cleanup);

const here = dirname(fileURLToPath(import.meta.url));
const contactsSrc = () =>
	readFileSync(resolve(here, '..', '..', 'routes', 'contacts', '+page.svelte'), 'utf8');
const browseSrc = () =>
	readFileSync(resolve(here, '..', '..', 'routes', 'browse', '+page.svelte'), 'utf8');
const dragGroupSrc = () =>
	readFileSync(resolve(here, '..', '..', 'lib', 'drag-group.ts'), 'utf8');

const GROUPS: Group[] = [
	{ name: 'Film', pubkeys: [], color: '#e05c5c' },
	{ name: 'Music', pubkeys: [], color: '#4d8fe0' },
];

interface PopoverProps {
	open: boolean;
	anchor?: HTMLElement;
	contactName: string;
	groups: Group[];
	memberships: string[];
	onapply: (selected: string[]) => void;
	onclose: () => void;
	onnewgroup: () => void;
	returnFocusTo?: () => HTMLElement | undefined | null;
}

interface PopoverMocks extends PopoverProps {
	onapply: ReturnType<typeof vi.fn>;
	onclose: ReturnType<typeof vi.fn>;
	onnewgroup: ReturnType<typeof vi.fn>;
}

function makePopover(overrides: Partial<PopoverProps> = {}): PopoverMocks {
	return {
		open: true,
		anchor: undefined,
		contactName: 'Alice',
		groups: GROUPS,
		memberships: ['Film'],
		onapply: vi.fn(),
		onclose: vi.fn(),
		onnewgroup: vi.fn(),
		...overrides,
	} as PopoverMocks;
}

describe('GroupMembershipPopover (M22 W8) — behaviour', () => {
	it('renders a checkbox per group, pre-checked from current memberships', () => {
		const { getAllByRole } = render(GroupMembershipPopover, { props: makePopover() });
		const boxes = getAllByRole('checkbox') as HTMLInputElement[];
		expect(boxes.length).toBe(2);
		expect(boxes[0].checked).toBe(true); // Film — currently a member
		expect(boxes[1].checked).toBe(false); // Music — not a member
	});

	it('toggling a checkbox updates the draft', async () => {
		const { getAllByRole } = render(GroupMembershipPopover, { props: makePopover() });
		const boxes = getAllByRole('checkbox') as HTMLInputElement[];
		await fireEvent.click(boxes[1]); // check Music
		expect(boxes[1].checked).toBe(true);
		await fireEvent.click(boxes[0]); // uncheck Film
		expect(boxes[0].checked).toBe(false);
	});

	it('Apply emits the FULL checked set (not a single name — the data-loss guard)', async () => {
		const p = makePopover();
		const { getByRole } = render(GroupMembershipPopover, { props: p });
		// Add Music too → Apply must send ['Film', 'Music'].
		const boxes = document.querySelectorAll('input[type="checkbox"]') as NodeListOf<HTMLInputElement>;
		await fireEvent.click(boxes[1]);
		await fireEvent.click(getByRole('button', { name: 'Apply' }));
		expect(p.onapply).toHaveBeenCalledTimes(1);
		const emitted = p.onapply.mock.calls[0][0] as string[];
		expect(emitted).toEqual(expect.arrayContaining(['Film', 'Music']));
		expect(emitted.length).toBe(2);
	});

	it('Cancel closes without calling onapply', async () => {
		const p = makePopover();
		const { getByRole } = render(GroupMembershipPopover, { props: p });
		await fireEvent.click(getByRole('button', { name: 'Cancel' }));
		expect(p.onclose).toHaveBeenCalledTimes(1);
		expect(p.onapply).not.toHaveBeenCalled();
	});

	it('"+ New group…" routes to onnewgroup (the caller\'s CreateGroupDialog)', async () => {
		const p = makePopover();
		const { getByText } = render(GroupMembershipPopover, { props: p });
		await fireEvent.click(getByText('+ New group…'));
		expect(p.onnewgroup).toHaveBeenCalledTimes(1);
	});

	it('renders the colour dot for a group that has one (absent ⇒ no dot)', () => {
		const { container } = render(GroupMembershipPopover, { props: makePopover() });
		expect(container.querySelectorAll('.gmp-dot').length).toBe(2);
		const noColour = makePopover({ groups: [{ name: 'Bare', pubkeys: [] }] });
		const { container: c2 } = render(GroupMembershipPopover, { props: noColour });
		expect(c2.querySelectorAll('.gmp-dot').length).toBe(0);
	});
});

describe('GroupMembershipPopover (M22 W8) — the two consumers converge on the same write', () => {
	it('BOTH pages render the shared component (not a copy)', () => {
		expect(contactsSrc()).toMatch(/GroupMembershipPopover from '\$lib\/components\/GroupMembershipPopover\.svelte'/);
		expect(browseSrc()).toMatch(/GroupMembershipPopover from '\$lib\/components\/GroupMembershipPopover\.svelte'/);
		// The component source is the only place the checkbox editor markup lives — neither page
		// carries a second, parallel copy of the editor.
		expect(contactsSrc()).not.toMatch(/class="gp-row"/);
		expect(contactsSrc()).not.toMatch(/groupPopoverDraft/);
		expect(browseSrc()).not.toMatch(/class="gp-row"/);
		expect(browseSrc()).not.toMatch(/groupPopoverDraft/);
	});

	it('Contacts\' Apply forwards the FULL set to contactUpdateGroups (full-set replace)', () => {
		const s = contactsSrc();
		const fn = s.slice(s.indexOf('async function applyGroupPopover'), s.indexOf('// M20 W2:'));
		expect(fn).toMatch(/contactUpdateGroups\(npub, names\)/);
		expect(fn).not.toMatch(/\[newGroupName\]/);
	});

	it('Browse\'s Apply forwards the FULL set to contactUpdateGroups — the same command', () => {
		const s = browseSrc();
		const fn = s.slice(s.indexOf('async function applyGroupPopover'), s.indexOf('async function loadGroupsInto'));
		expect(fn).toMatch(/contactUpdateGroups\(npub, names\)/);
		expect(fn).not.toMatch(/\[newGroupName\]/);
	});

	it('the component\'s only write path is the onapply callback — it never names the audience API', () => {
		const p = readFileSync(resolve(here, 'GroupMembershipPopover.svelte'), 'utf8');
		expect(p).toMatch(/onapply\(\[\.\.\.draft\]\)/);
		expect(p).not.toMatch(/privateAudienceList/);
		expect(p).not.toMatch(/privateAudienceSet/);
		expect(p).not.toMatch(/privateAudience/);
	});

	it('the drop-write primitives converge on the same command and never touch the audience (drag-group.ts)', () => {
		const src = dragGroupSrc();
		// The shared primitives that BOTH pages route their drag paths through.
		expect(src).toMatch(/contactUpdateGroups\(sourceNpub, \[outcome\.target\]\)/); // move
		expect(src).toMatch(/contactUpdateGroups\(sourceNpub, \[\]\)/); // ungrouped
		expect(src).not.toMatch(/privateAudience/);
	});
});
