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
	| 'custom-relays'
	| 'network-type';

/** The canonical key list — single source of truth for iteration + the registry-completeness test. */
export const TOOLTIP_KEYS: TooltipKey[] = [
	'no-download',
	'willing-to',
	'listings-locked',
	'k-of-n-folders',
	'fingerprint',
	'custom-relays',
	'network-type',
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
		body: 'Hoardbook moves no files. It finds people and shows what they have. Arrange the transfer yourselves: a DM, their contact hint, or whatever you both already use.',
	},
	'willing-to': {
		title: 'Willing to',
		body: 'How this hoarder prefers to arrange an exchange: seed a torrent, trade, upload a copy, or meet up in person. Hoardbook moves no files, so these are hints, not buttons.',
	},
	// Spec verbatim: "you have their npub but not their share code."
	'listings-locked': {
		title: 'Listings locked',
		body: 'You have their npub but not their share code, so their listings stay sealed. Ask them for the share code to browse what they have.',
	},
	'k-of-n-folders': {
		title: 'K of N folders available',
		body: 'Large listings travel in parts, one per folder. Some parts are missing, withheld by the owner or not yet returned by a relay, so you are seeing only some of the folders.',
	},
	'fingerprint': {
		title: 'Identity fingerprint',
		body: 'A word-and-color fingerprint of this person’s key. It follows the key, not the display name, so a copycat reusing the same name shows a different fingerprint.',
	},
	// M23 W4 (QURATOR-75): corrects the "more relays = more reach" misconception. A relay nobody
	// else dials is a private room, not a megaphone — the defaults are where strangers see you.
	'custom-relays': {
		title: 'Custom relays',
		body: 'Add a relay to build a private community where you and the people you tell will meet. It will not widen your reach: nobody else connects to your relay by default. The defaults are where strangers see you.',
	},
	// Owner, 2026-08-27: "add a tooltip letting the user know what it means if they're being a NAT."
	// Says what it COSTS them, not what it is — a definition of network address translation helps
	// nobody decide anything. Deliberately does not overclaim: the classifier's own copy calls CGNAT
	// "a strong signal, not proof" (QURATOR-68), so this must not promise that direct transfer will
	// fail, only that it may need a relay. Everything except the direct manifest hand-off works
	// identically behind any NAT, and saying so is the point — otherwise the pill reads as a fault.
	'network-type': {
		title: 'Network type',
		body: 'How your connection sits behind your router. Behind NAT is normal. CGNAT means your provider funnels many customers through one address, so sending someone a full collection list may need a relay, or fail if neither side is reachable. Browsing, chat, Topics, and presence work the same either way.',
	},
};
