// Feature tooltips (hover-to-learn) content registry (M8, HOARDBOOK_SPEC §8). Pure, typed copy so
// the registry is unit-tested and the <FeatureTooltip> component stays thin. These are EXPLANATORY
// ONLY — a tooltip never gates an action (spec). A drift guard test pins the exact key count so
// nobody silently sprinkles feature-help app-wide. Distinct from per-item *notes*, which are
// content, not feature help.
//
// Owner ruling 2026-09-26: the Settings "Preferences" toggles' long explanatory sub-copy moved
// here (allow-dms, auto-update-snapshots, reconcile-poll, discoverable) — each toggle keeps a
// short one-line label/sub-label on the page and the fuller explanation lives behind its (i).
// "Fetch new collections automatically" (swarm-caching) is the deliberate exception: its
// reciprocity disclosure stays visible on the page (owner ruling, QURATOR-164) — shortened, not
// hidden — because hiding "other people will request from you too" behind a hover would soften a
// consent-relevant fact the owner ruled must be stated plainly.

export type TooltipKey =
	| 'no-download'
	| 'willing-to'
	| 'listings-locked'
	| 'k-of-n-folders'
	| 'fingerprint'
	| 'custom-relays'
	| 'network-type'
	| 'allow-dms'
	| 'auto-update-snapshots'
	| 'reconcile-poll'
	| 'discoverable';

/** The canonical key list — single source of truth for iteration + the registry-completeness test. */
export const TOOLTIP_KEYS: TooltipKey[] = [
	'no-download',
	'willing-to',
	'listings-locked',
	'k-of-n-folders',
	'fingerprint',
	'custom-relays',
	'network-type',
	'allow-dms',
	'auto-update-snapshots',
	'reconcile-poll',
	'discoverable',
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
		body: 'How your connection sits behind your router. NAT is normal. CGNAT means your provider funnels many customers through one address, so sending someone a full collection list may need a relay, or fail if neither side is reachable. Browsing, chat, Topics, and presence work the same either way.',
	},
	'allow-dms': {
		title: 'Allow incoming messages from anyone',
		body: 'Off means only your contacts can DM you. Strangers can still ask, but their message waits as a Request until you accept, decline, or block.',
	},
	'auto-update-snapshots': {
		title: 'Auto-update snapshots on change',
		body: 'Re-publishes a collection automatically when its folder changes. Off means only a manual rescan updates it. Either way, edits made from another computer on a network share are picked up at launch.',
	},
	// Owner ruling 2026-09-26: rewritten to say what it DOES at a glance, not what it's for. The
	// old copy ("Low-frequency re-check for collections you edit from another host (SMB)") named
	// the audience before the mechanism; this leads with the verb.
	'reconcile-poll': {
		title: 'Reconcile poll',
		body: 'Periodically re-checks your published collections for changes, on top of the normal file watcher. Turn this on only if you edit them from another computer over a network share (SMB) — the local watcher alone won’t see those edits.',
	},
	'discoverable': {
		title: 'Show up in Discover Hoarders',
		body: 'Off means people can’t find you by tag or content-type search. They can still reach you with your npub or share code, and your contacts are unaffected.',
	},
};
