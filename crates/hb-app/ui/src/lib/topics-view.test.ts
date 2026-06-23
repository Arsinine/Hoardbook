import { describe, it, expect } from 'vitest';
import {
	joinConsentCopy,
	canJoin,
	contactBadge,
	memberCountLabel,
	isDissolved,
	PUBLIC_JOIN_CONSENT,
	PRIVATE_JOIN_CONSENT,
	NO_UNLOCK_NOTE,
} from './topics-view.js';

describe('topics-view (M11)', () => {
	it('shows the public consent copy for a public Topic, the durable-record copy for a private one', () => {
		expect(joinConsentCopy(false)).toBe(PUBLIC_JOIN_CONSENT);
		expect(joinConsentCopy(true)).toBe(PRIVATE_JOIN_CONSENT);
		// The public copy makes the visibility explicit; the private copy names the durable record.
		expect(PUBLIC_JOIN_CONSENT.toLowerCase()).toContain('anyone who joins');
		expect(PRIVATE_JOIN_CONSENT.toLowerCase()).toContain('durable');
		expect(PRIVATE_JOIN_CONSENT.toLowerCase()).toContain('membership record');
	});

	it('F12: the join gate requires an explicit acknowledgment', () => {
		expect(canJoin(false)).toBe(false);
		expect(canJoin(true)).toBe(true);
	});

	it('badges only Topic-sourced contacts (manual adds get no badge)', () => {
		expect(contactBadge('Topic')).toBe('Topic');
		expect(contactBadge('Manual')).toBeNull();
		expect(contactBadge(undefined)).toBeNull(); // a pre-M11 contact ⇒ Manual ⇒ no badge
	});

	it('renders the member count as an approximate estimate, never a hard number', () => {
		expect(memberCountLabel(1)).toBe('~1 member (estimate)');
		expect(memberCountLabel(5)).toBe('~5 members (estimate)');
		expect(memberCountLabel(0)).toBe('~0 members (estimate)');
		expect(memberCountLabel(-3)).toBe('~0 members (estimate)'); // clamps junk
	});

	it('derives dissolution from an empty roster', () => {
		expect(isDissolved(0)).toBe(true);
		expect(isDissolved(2)).toBe(false);
	});

	it('the no-unlock note states INV-2 plainly', () => {
		expect(NO_UNLOCK_NOTE.toLowerCase()).toContain('does not unlock');
		expect(NO_UNLOCK_NOTE.toLowerCase()).toContain('share code');
	});
});
