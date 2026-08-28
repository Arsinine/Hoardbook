// M17 W8 — "two small truths". Both items in this workstream are COPY defects: the engine was
// already right in each case, and the words were describing something the app does not do.
//
// W8.1 (devtest item 1) — the owner reported that topic posts "wipe at a set time" rather than
// expiring per message. The engine was never wrong: `POST_TTL_SECS` is applied per post, each post
// carries its own NIP-40 expiration at its own ts + 24h, and the read side drops items individually
// (pinned in Rust by `staggered_post_expiry_drops_only_the_post_older_than_24h`, which passes
// against the UNCHANGED engine). The *copy* said "posts wipe after 24h", which reads as a scheduled
// collective sweep — and that is the mental model the owner reported back. So the fix is the words.
//
// W8.2 (devtest item 3) — the contact-hint copy suggested Reddit as the example channel. Copy-only
// by decision: the `reddit` entry in SOCIAL_PLATFORMS and its icon STAY, because an existing profile
// may already carry a stored `reddit` social link and dropping the option would orphan it. That is a
// data question, deliberately not answered here.

import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { extractUserFacingSegments } from './copy-audit.js';

const chatSrc = () =>
	readFileSync(new URL('../routes/chat/+page.svelte', import.meta.url), 'utf8');
const homeSrc = () => readFileSync(new URL('../routes/+page.svelte', import.meta.url), 'utf8');

describe('W8.1 — topic-channel copy describes PER-MESSAGE expiry, not a collective wipe', () => {
	it('the channel subtitle no longer says posts "wipe"', () => {
		// "wipe" is the word that carries the scheduled-sweep implication.
		const copy = extractUserFacingSegments(chatSrc()).join('\n');
		expect(copy).not.toMatch(/posts wipe/i);
		expect(copy).toContain("each post disappears 24h after it's posted");
	});

	it('the empty state explains per-message expiry rather than implying a window sweep', () => {
		const copy = extractUserFacingSegments(chatSrc()).join('\n');
		// The production line is "posts here expire 24h after they're sent" (lowercase, mid-sentence).
		// 202a4e0 rewrote this assertion to the capitalised "Posts expire…" but never touched the
		// page itself, so it stopped matching — pin the string the page actually renders.
		expect(copy).toContain("posts here expire 24h after they're sent");
	});

	it('both strings attribute the 24h to the POST, not to a clock', () => {
		// The distinguishing test: the copy must tie the deadline to when a post was sent
		// ("after it's posted" / "after they're sent"), never to a time of day or a shared boundary.
		const copy = extractUserFacingSegments(chatSrc()).join('\n');
		for (const phrase of ["after it's posted", "after they're sent"]) {
			expect(copy).toContain(phrase);
		}
		// Guard the regression shape directly: no scheduled/collective vocabulary near the 24h.
		expect(copy).not.toMatch(/wipe[sd]? (at|after|every)/i);
		expect(copy).not.toMatch(/cleared (at|every)/i);
	});
});

describe('W8.2 — the contact hint no longer suggests Reddit', () => {
	it('neither the hint text nor the placeholder mentions Reddit or a u/ handle', () => {
		// Scoped to the contact-hint FIELD, not the whole page: the social-links picker keeps a
		// legitimately user-facing "Reddit" option label (see the last test in this block). A
		// page-wide sweep here would be testing the opposite of the decision that was made.
		const src = homeSrc();
		const start = src.indexOf('Contact hint');
		expect(start).toBeGreaterThan(-1);
		const field = src.slice(start, src.indexOf('</div>', start));
		expect(field).not.toMatch(/reddit/i);
		expect(field).not.toMatch(/\bu\//);
	});

	it('the hint still names concrete alternatives (the field stays self-explanatory)', () => {
		// Removing the example must not leave the field meaningless — the point of the hint is to
		// show what shape of thing belongs here.
		const copy = extractUserFacingSegments(homeSrc()).join('\n');
		expect(copy).toContain('a Discord or Matrix handle or an email');
		expect(copy).toContain('you@example.com');
	});

	it('the `reddit` SOCIAL_PLATFORMS entry is deliberately KEPT (copy-only decision)', () => {
		// Not an oversight: a stored profile may already carry a `reddit` social link, and removing
		// the platform would orphan it. The contact hint and the social-links picker are separate
		// surfaces; only the former was the reported defect. If this ever changes, the removal owes
		// a migration for already-stored links — see W8.2 in planning/M17_PROMPT.md.
		expect(homeSrc()).toMatch(/['"]reddit['"]/);
	});
});
