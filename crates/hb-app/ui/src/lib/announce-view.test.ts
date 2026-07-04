import { describe, expect, it } from 'vitest';
import { canAnnounce, cooldownLabel, ANNOUNCE_EXPLAINER } from './announce-view.js';

describe('announce-view', () => {
	it('canAnnounce is true at exactly zero remaining', () => {
		expect(canAnnounce(0)).toBe(true);
	});

	it('canAnnounce is false for any positive remainder', () => {
		expect(canAnnounce(1)).toBe(false);
		expect(canAnnounce(3600)).toBe(false);
	});

	it('label is plain "Announce" when ready', () => {
		expect(cooldownLabel(0)).toBe('Announce');
	});

	it('label shows minutes remaining, ceiling-rounded', () => {
		expect(cooldownLabel(61)).toBe('Announce — ready in 2 min');
		expect(cooldownLabel(120)).toBe('Announce — ready in 2 min');
	});

	it('label floors at 1 min for any sub-minute remainder', () => {
		expect(cooldownLabel(1)).toBe('Announce — ready in 1 min');
		expect(cooldownLabel(59)).toBe('Announce — ready in 1 min');
	});

	it('matches the spec example verbatim (47 min remaining)', () => {
		expect(cooldownLabel(47 * 60)).toBe('Announce — ready in 47 min');
	});

	it('ANNOUNCE_EXPLAINER names the audience, duration, and frequency limit', () => {
		expect(ANNOUNCE_EXPLAINER).toContain('24h');
		expect(ANNOUNCE_EXPLAINER).toContain('one per hour');
	});
});
