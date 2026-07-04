import { describe, expect, it } from 'vitest';
import { contactDisplayName, shortNpub } from './contact-display.js';

const NPUB = 'npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqabcd';

describe('contact-display', () => {
	it('prefers_petname', () => {
		expect(
			contactDisplayName({ npub: NPUB, petname: 'Al', profile: { display_name: 'Alice' } }),
		).toBe('Al');
	});

	it('falls_back_display_then_short_npub', () => {
		// No petname ⇒ the published display_name.
		expect(contactDisplayName({ npub: NPUB, profile: { display_name: 'Alice' } })).toBe('Alice');
		// Neither ⇒ a short npub, never a blank / "Unknown".
		expect(contactDisplayName({ npub: NPUB })).toBe(shortNpub(NPUB));
		expect(contactDisplayName({ npub: NPUB, petname: '', profile: undefined })).toBe(shortNpub(NPUB));
	});

	it('ignores whitespace-only petname/display_name', () => {
		expect(contactDisplayName({ npub: NPUB, petname: '   ', profile: { display_name: 'Alice' } })).toBe(
			'Alice',
		);
		expect(contactDisplayName({ npub: NPUB, petname: '  ', profile: { display_name: '  ' } })).toBe(
			shortNpub(NPUB),
		);
	});

	it('shortNpub truncates long npubs and leaves short ones alone', () => {
		expect(shortNpub(NPUB)).toBe(`${NPUB.slice(0, 8)}…${NPUB.slice(-4)}`);
		expect(shortNpub('npub1x')).toBe('npub1x');
	});
});
