import { describe, it, expect } from 'vitest';
import type { Group } from './types.js';
import {
	DEFAULT_VISIBILITY,
	NOT_DRM_NOTE,
	visibilityOf,
	isTrusted,
	trustedRecipients,
	contactIsTrusted,
} from './private-collections-view.js';

const grp = (name: string, pubkeys: string[], trusted?: boolean): Group => ({
	name,
	pubkeys,
	...(trusted === undefined ? {} : { trusted }),
});

describe('Private Collections view-model (M10)', () => {
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

	it('isTrusted defaults to false for a pre-M10 group', () => {
		expect(isTrusted({ trusted: undefined })).toBe(false);
		expect(isTrusted({ trusted: false })).toBe(false);
		expect(isTrusted({ trusted: true })).toBe(true);
	});

	it('trustedRecipients unions + dedups trusted groups and ignores untrusted ones', () => {
		const groups = [
			grp('inner', ['npub_a', 'npub_b'], true),
			grp('also', ['npub_a', 'npub_c'], true), // npub_a duplicated across trusted groups
			grp('acquaintances', ['npub_x'], false), // untrusted → excluded
			grp('legacy', ['npub_y']), // no `trusted` field ⇒ untrusted
		];
		expect(trustedRecipients(groups).sort()).toEqual(['npub_a', 'npub_b', 'npub_c']);
	});

	it('contactIsTrusted is true only for members of a trusted group', () => {
		const groups = [grp('inner', ['npub_a'], true), grp('friends', ['npub_b'], false)];
		expect(contactIsTrusted('npub_a', groups)).toBe(true);
		expect(contactIsTrusted('npub_b', groups)).toBe(false); // in an untrusted group
		expect(contactIsTrusted('npub_z', groups)).toBe(false); // in no group
	});
});
