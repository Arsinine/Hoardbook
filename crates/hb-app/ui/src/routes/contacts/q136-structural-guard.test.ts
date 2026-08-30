// QURATOR-136 structural guard (node environment — jsdom forbids `file:` URL reads, which is why
// this is a separate file from the mounted behavioural tests in q136-search-published-fields.test.ts).
//
// THE RULE THIS GUARDS: "if the contact card shows a field, search should find it." Enforced
// STRUCTURALLY, not by listing sites: the expected field set is DERIVED from +page.svelte's own
// detail-card template (every `p.<field>` token in the "Seven Profile fields" block), so a field
// added to the card without being added to `profileHaystack` reds this test naming the field — no
// hand-maintained list of field names to drift (CLAUDE.md §9: a hand-written list of sites cannot
// fix a hand-written list of sites).
//
// The deliberate exclusions are stated as literals HERE so relaxing one is a visible edit, not a
// silent one. MUTATION PROOF (§9): the "every card field is searched or excluded" test was run
// against the pre-fix profileHaystack (which omitted location/contact_hint/languages/willing_to)
// and reds naming exactly those fields.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { matchesQuery } from '$lib/contacts-view.js';
import type { CachedPeer, Profile } from '$lib/types.js';

const page = readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');
const start = page.indexOf('Seven Profile fields');
const block = page.slice(start, page.indexOf('</dl>', start));

// Every `p.<word>` token the card template reads (each field appears more than once — read +
// render — so dedup). This is the derived, hand-maintenance-free field set.
const rendered = [...new Set([...block.matchAll(/\bp\.([a-z_][a-z0-9_]*)/g)].map((m) => m[1]))];

// Deliberate exclusions, each with its reason:
// - since / est_size / picture / updated — a year, a size label, an avatar and a timestamp are not
//   things anyone types into a search box; searching them invites accidental partial matches
//   ("202" would hit everyone's since-year).
// - email — the owner opted this in for hand-copying from the card (its type comment says
//   "Publicly visible — user explicitly opts in by filling this field"); search is not the place
//   to enumerate who has an address on file.
const NOT_SEARCHED = new Set(['since', 'est_size', 'picture', 'updated', 'email']);

const BASE_PROFILE: Profile = {
	display_name: 'Unremarkable Person',
	bio: 'nothing distinctive here',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

function peer(profile: Profile): CachedPeer {
	return {
		npub: 'npub1q136' + 'a'.repeat(50),
		collections: [],
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: [],
		profile,
	};
}

describe('QURATOR-136 structural guard — the haystack covers every field the card renders', () => {
	it('the card block was found (the template slice is not empty)', () => {
		expect(start).toBeGreaterThan(0);
		expect(rendered.length).toBeGreaterThan(3);
	});

	it('every card field is either searched by matchesQuery or on the stated exclusion list', () => {
		// Each scalar/object field the card renders gets a UNIQUE marker in one probe peer; if
		// matchesQuery drops it, its marker stops matching and this failure names the field.
		const markers: Record<string, string> = {
			location: 'zzq-region',
			languages: 'zzq-language',
			willing_to: 'zzq-willing',
			contact_hint: 'zzq-hint',
			// social_links: the card renders each link's platform + handle. The handle is a
			// peer-controlled contact string (same class as contact_hint); the platform label
			// ("github", "matrix") is a low-value enum. Searching the handle is the meaningful
			// half — same "the card shows it" rule as contact_hint.
			social_links: 'zzq-social',
		};
		const probe = peer({
			...BASE_PROFILE,
			location: markers.location,
			languages: [markers.languages],
			willing_to: [markers.willing_to],
			contact_hint: markers.contact_hint,
			tags: ['zzq-theirtag'],
			content_types: ['zzq-ct'],
			social_links: [{ platform: 'matrix', handle: markers.social_links }],
		});
		for (const field of rendered) {
			if (NOT_SEARCHED.has(field)) continue;
			const marker = markers[field] ?? (field === 'tags' ? 'zzq-theirtag' : undefined);
			expect(marker, `card field ${field} needs a probe marker — add one`).toBeTruthy();
			expect(matchesQuery(probe, marker!), `card field ${field} must be searchable`).toBe(true);
		}
		// The stated exclusions stay excluded (a pass on a marker means the exclusion was relaxed —
		// fine, but move the field off NOT_SEARCHED so this stays honest).
		expect(matchesQuery(peer({ ...BASE_PROFILE, email: 'zzq-email@example.net' }), 'zzq-email'), 'email is deliberately not searched').toBe(false);
		// Vacuousness guard: none of the markers appear anywhere in the base fixture.
		expect(matchesQuery(peer({ ...BASE_PROFILE }), 'zzq-')).toBe(false);
	});
});
