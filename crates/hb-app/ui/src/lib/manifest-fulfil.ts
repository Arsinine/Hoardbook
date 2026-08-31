// M17 W7.1b — the manifest-request fulfilment card (the SECOND structured render in chat, after
// W3's ShareCodeCard). The request a browser DMs ("Asking for the full list of …") renders in Chat
// as a bare text bubble with NO verb. The `export_manifest` capability is fully wired but lives
// behind Home → ⋯ → Export → "Manifest file (.hbmanifest)" — a submenu, inside an overflow menu,
// on a row, on a DIFFERENT TAB. This helper derives the card's state purely from (request, own
// drafts) so the chat page can surface the export action exactly where the request lands.
//
// Same rules as W3's ShareCodeCard:
//   - local parse only (`parseManifestRequest` already exists, already pure).
//   - zero network on render — the helper is pure; the export Tauri call fires only on click.
//   - quarantine (Q7 request inbox) renders ZERO action buttons; Accept comes first, always.
//
// Backend guards mirrored here as UI state (do NOT surface a button guaranteed to fail):
//   - Private collection rejected at `collection.rs:713` → inert explanatory line, no button.
//   - Empty `content_types` rejected at `collection.rs:721` → inert line pointing at the fix.
//
// Headline failure modes guarded here:
//   - "the button appears for a collection that's Private / empty / missing" → the UI says what's
//     wrong BEFORE the click, never surfaces a backend error after.
//   - "the card refuses a fresh manifest just because the browser saw an older fingerprint" →
//     staleness is information, not a gate; the export still fires, with a note.
//   - "the card offers to send the file" → it never does (MAS-INV-5 + INV-4). The post-export copy
//     says Hoardbook moves no files — send it yourself.

import type { Collection } from './types.js';
import { parseManifestRequest, type ManifestRequest } from './request-inbox.js';
import { shortNpub } from './contact-display.js';

/** The card states (five from spec W7.1b, plus carrier 4's `re-serve`). Pure derivation from
 *  (request, own drafts).
 *
 *  Carrier 4 (QURATOR-79): `authorNpub` on a state is the third-party author the request named, or
 *  undefined for the asked peer's own collection (the only shape before carrier 4). It is carried,
 *  not acted on, by the derivation — the re-serve decision belongs to the human reading the card,
 *  and the state machine's job is to make sure the card says WHOSE list is being asked for. */
export type ManifestFulfilState =
	| { kind: 'public'; slug: string; stale: boolean; authorNpub?: string }
	| { kind: 'private'; slug: string; authorNpub?: string }
	| { kind: 'empty'; slug: string; authorNpub?: string }
	| { kind: 'missing'; slug: string; authorNpub?: string }
	| { kind: 'quarantine'; slug: string; authorNpub?: string }
	| { kind: 're-serve'; slug: string; authorNpub: string };

/** The card's resolution of a request against the owner's own drafts. `quarantined=true` short-
 *  circuits to the inert state regardless of slug match (Accept first, always — same rule as W3).
 *
 *  `drafts` is the owner's `getCollections()` result. A collection with `visibility` absent is
 *  treated as Public (pre-M10 default — never mis-render old drafts as something they're not).
 *
 *  Staleness: when a matched draft carries `snapshot_fingerprint` AND the request's
 *  `fingerprintSeen` differs from it, the state is `public` with `stale=true` — the export still
 *  fires (a fresh manifest is what they want); the card surfaces a "they saw an older version" note.
 *  Never a gate.
 */
export function deriveManifestFulfil(
	request: ManifestRequest,
	drafts: Collection[],
	opts: { quarantined: boolean },
): ManifestFulfilState {
	const slug = request.slug;
	const authorNpub = request.authorNpub;
	// Quarantine short-circuits FIRST, as ever — Accept comes before any state is judged (same rule
	// as W3). What the quarantined card renders is the hint's copy (request-inbox.ts), which already
	// names the third-party author, so the pre-Accept read stays honest without a verb here.
	if (opts.quarantined) return { kind: 'quarantine', slug };
	// A third-party-author ask is a RE-SERVE: the peer wants an envelope this owner never authored —
	// it lives in the owner's manifest cache (put there by browsing that author), not in the drafts
	// the slug-match below walks. So the own-draft resolution only applies to the own-collection ask;
	// a re-serve ask is always derived as `re-serve` and judged by the human on the card.
	if (authorNpub) return { kind: 're-serve', slug, authorNpub };
	const draft = drafts.find((c) => c.slug === slug);
	if (!draft) return { kind: 'missing', slug };
	// Absent visibility ⇒ Public (pre-M10 default). Only an EXPLICIT 'Private' is refused.
	if (draft.visibility === 'Private') return { kind: 'private', slug };
	if (draft.content_types.length === 0) return { kind: 'empty', slug };
	const stale =
		request.fingerprintSeen !== '' &&
		draft.snapshot_fingerprint !== undefined &&
		request.fingerprintSeen !== draft.snapshot_fingerprint;
	return { kind: 'public', slug, stale };
}

/** Parse a chat message into a fulfil state (or null for an ordinary message). Pure; wraps
 *  `parseManifestRequest` + `deriveManifestFulfil` so the chat route can call one function. */
export function manifestFulfilFor(
	content: string,
	drafts: Collection[],
	opts: { quarantined: boolean },
): { request: ManifestRequest; state: ManifestFulfilState } | null {
	const req = parseManifestRequest(content);
	if (!req) return null;
	return { request: req, state: deriveManifestFulfil(req, drafts, opts) };
}

// ── Copy (single source — the chat route and the tests read from here) ───────────────────────

/** Post-export toast/inline note. Honest about the last mile: Hoardbook writes the file and moves
 *  no bytes (MAS-INV-5 + INV-4). The owner is the courier — "send it to them yourself" is the
 *  instruction, never a Hoardbook-side Send affordance (no button, no auto-deliver). */
export const MANIFEST_EXPORTED_TOAST = (filename: string) =>
	`Saved ${filename}. Hoardbook moves no files. Send it yourself.`;

/** The Private inert line. Mirrors the backend's `collection.rs:713` refusal BEFORE the click. */
export const MANIFEST_PRIVATE_LINE =
	"Private collections are already sent whole to trusted contacts. There's no manifest to export.";

/** The empty-content-types inert line. Mirrors the backend's `collection.rs:721` refusal. */
export const MANIFEST_EMPTY_LINE =
	"This collection has no content types yet. Add one first.";

/** The missing-draft inert line. Rendered when the slug no longer matches any of your drafts. */
export const MANIFEST_MISSING_LINE = (slug: string) =>
	`You don't have a collection called “${slug}” any more.`;

/** The stale-fingerprint note. The export still fires (a fresh manifest is what they want). */
export const MANIFEST_STALE_NOTE =
	'They saw an older version. You will be sending the current one.';

/** The big-relay hint, shown when the owner has a `big_relay_url` configured. */
export const MANIFEST_BIG_RELAY_HINT =
	'Or publish to your big relay. They get the rest automatically.';

/** The muted one-liner linking to the Settings field, when no big relay is configured. */
export const MANIFEST_BIG_RELAY_LINK = 'Add a big relay in Settings to publish the rest for them.';

/** Carrier 4 (QURATOR-79) — the re-serve ask's inert explanatory line, shown when the request names
 *  a third-party author. The peer is asking for someone else's list from this owner's cache: the
 *  serve is a cache read (not a build), and only the owner can say whether they still hold a copy.
 *  Kept honest in the family voice: it names the LIST as what crosses, never the files (MAS-INV-5 +
 *  INV-4, same as every line above). */
export const MANIFEST_RESERVE_LINE = (authorNpub: string) =>
	`They're asking for a list ${shortNpub(authorNpub)} made — say the word and it goes from your cache.`;
