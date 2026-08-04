import { describe, it, expect } from 'vitest';
import {
	DEFAULT_VISIBILITY,
	NOT_DRM_NOTE,
	visibilityOf,
	audienceRecipients,
	receivesPrivate,
} from './private-collections-view.js';

describe('Private Collections view-model (M21 W5)', () => {
	it('defaults visibility to Public, never silently Private', () => {
		expect(DEFAULT_VISIBILITY).toBe('Public');
		expect(visibilityOf({ visibility: undefined })).toBe('Public'); // pre-M10 collection
		expect(visibilityOf({ visibility: 'Private' })).toBe('Private');
		expect(visibilityOf({ visibility: 'Public' })).toBe('Public');
	});

	it('the not-DRM note states both honest caveats (copy + future-only revoke)', () => {
		expect(NOT_DRM_NOTE.toLowerCase()).toContain('not drm');
		expect(NOT_DRM_NOTE.toLowerCase()).toContain('copy');
		// Must say revoke is future-only — never imply a recall.
		expect(NOT_DRM_NOTE.toLowerCase()).toContain('future republishes');
	});

	it('audienceRecipients dedups the explicit audience list', () => {
		// M21 W5: the audience is a plain list of npubs — no group affiliation involved.
		const audience = ['npub_a', 'npub_b', 'npub_a', 'npub_c'];
		expect(audienceRecipients(audience).sort()).toEqual(['npub_a', 'npub_b', 'npub_c']);
	});

	it('receivesPrivate is true only for npubs explicitly in the audience', () => {
		const audience = ['npub_a', 'npub_b'];
		expect(receivesPrivate('npub_a', audience)).toBe(true);
		expect(receivesPrivate('npub_b', audience)).toBe(true);
		expect(receivesPrivate('npub_z', audience)).toBe(false); // not in the audience
	});
});
