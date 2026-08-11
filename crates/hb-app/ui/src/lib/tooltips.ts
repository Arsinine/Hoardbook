// Feature tooltips (hover-to-learn) content registry (M8, HOARDBOOK_SPEC §8). Pure, typed copy so
// the registry is unit-tested and the <FeatureTooltip> component stays thin. These are EXPLANATORY
// ONLY — a tooltip never gates an action (spec). Six anchors, no more: a drift guard test pins the
// count so nobody silently sprinkles feature-help app-wide. Distinct from per-item *notes*, which
// are content, not feature help.

export type TooltipKey =
	| 'no-download'
	| 'willing-to'
	| 'listings-locked'
	| 'k-of-n-folders'
	| 'fingerprint'
	| 'custom-relays';

/** The canonical key list — single source of truth for iteration + the registry-completeness test. */
export const TOOLTIP_KEYS: TooltipKey[] = [
	'no-download',
	'willing-to',
	'listings-locked',
	'k-of-n-folders',
	'fingerprint',
	'custom-relays',
];

export interface TooltipContent {
	title: string;
	body: string;
}

export const TOOLTIPS: Record<TooltipKey, TooltipContent> = {
	// Lifts the spec's verbatim no-download copy (H4/INV-4′). Still true after M18: the transport
	// plane carries manifests (listings), never a user's collection files.
	'no-download': {
		title: 'No downloads here',
		body: 'Hoardbook moves no files; it finds people and shows what they have. Arrange the transfer off-platform — a DM, their contact hint, or whatever the two of you already use.',
	},
	'willing-to': {
		title: 'Willing to',
		body: 'How this hoarder prefers to arrange an off-platform exchange — seed (share via torrent), trade (swap), upload (send a copy), or meet up (hand off in person). Hoardbook moves no files, so these are hints, not buttons.',
	},
	// Spec verbatim: "you have their npub but not their share code."
	'listings-locked': {
		title: 'Listings locked',
		body: 'You have their npub but not their share code, so their listings stay sealed. Ask them for the share code to browse what they have.',
	},
	'k-of-n-folders': {
		title: 'K of N folders available',
		body: 'A large listing is split into per-folder parts. Some are missing — a part the owner withheld, or an oversize-split part a relay has not returned — so you are seeing only some of the folders.',
	},
	'fingerprint': {
		title: 'Identity fingerprint',
		body: 'A word-and-color fingerprint of this person’s key — your impersonation defense. It is bound to the npub, not the display name, so a copycat reusing the same name shows a different fingerprint.',
	},
	// M23 W4 (QURATOR-75): corrects the "more relays = more reach" misconception. A relay nobody
	// else dials is a private room, not a megaphone — the defaults are where strangers see you.
	'custom-relays': {
		title: 'Custom relays',
		body: 'Add a relay to build a private community — you and the people you tell will meet there. It does not make you easier to find: nobody else is connected to your relay by default, so adding more does not widen your reach. The defaults are where strangers will see you.',
	},
};
