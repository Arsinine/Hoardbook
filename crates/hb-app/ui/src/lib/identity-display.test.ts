import { describe, expect, it } from 'vitest';
import {
	petnameFor,
	renderFingerprint,
	strangerBadge,
	type Contact,
	type Fingerprint,
} from './identity-display.js';
import fixture from './fingerprint_vectors.json';

// The golden vectors are produced by the Rust fingerprint helper (hb_core::fingerprint) and pinned
// by its `fingerprint_matches_golden_vectors` test. The frontend RENDERS these values — it never
// re-derives the fingerprint (M3 decision #7). If the algorithm changes, regenerate this fixture.
const vectors = fixture.vectors as Array<{ npub: string; words: string[]; colorHex: string }>;

describe('AB4b — petname binds to the npub, not the display name', () => {
	const contacts: Contact[] = [{ npub: 'npub1alice', petname: 'Alice' }];

	it('exact npub match shows the petname, verified', () => {
		const label = petnameFor('npub1alice', 'whatever_they_call_themselves', contacts);
		expect(label).toEqual({ label: 'Alice', verified: true, stranger: false });
	});

	it('a stranger reusing a contact name under a different key is flagged, not trusted', () => {
		const label = petnameFor('npub1impostor', 'Alice', contacts);
		expect(label.verified).toBe(false);
		expect(label.stranger).toBe(false);
		expect(label.warning).toBe('not Alice — different key');
		expect(label.label).toBe('Alice'); // shows the claimed name, but flagged
	});

	it('an unknown key is an unverified stranger until followed', () => {
		const label = petnameFor('npub1nobody', 'archivebox_prime', contacts);
		expect(label.verified).toBe(false);
		expect(label.stranger).toBe(true);
		expect(strangerBadge(label)).toMatch(/unverified/);
		expect(strangerBadge({ ...label, stranger: false })).toBeNull();
	});
});

describe('AB4b — fingerprint rendering agrees with the Rust golden vectors', () => {
	it('renders deterministically and consumes (does not re-derive) the fixture', () => {
		expect(vectors.length).toBeGreaterThanOrEqual(2);
		for (const v of vectors) {
			const fp: Fingerprint = { words: v.words, colorHex: v.colorHex };
			const once = renderFingerprint(fp);
			const twice = renderFingerprint({ words: [...v.words], colorHex: v.colorHex });
			expect(once).toBe(twice); // deterministic for a given fingerprint
			expect(once).toContain(v.colorHex.toLowerCase());
			for (const w of v.words) expect(once).toContain(w);
		}
	});

	it('distinct keys render to distinct fingerprints (the distinguisher distinguishes)', () => {
		const rendered = vectors.map((v) => renderFingerprint({ words: v.words, colorHex: v.colorHex }));
		expect(new Set(rendered).size).toBe(rendered.length);
	});
});
