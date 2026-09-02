import type { ImportedManifest } from '$lib/api.js';

/** QURATOR-79 carrier 4 — the copy for an import result, provenance-aware. The old wording ("Ask
 *  the owner for a fresh manifest") was wrong twice under carrier 4: the owner is offline (that is
 *  *why* a peer re-served it), and the user could not tell who served this.
 *
 *  **Shared deliberately (QURATOR-172 #1).** This used to live inside `browse/+page.svelte`, which
 *  meant only Browse's import path could ever render it — and Browse's path gets `served_by: None`
 *  hardcoded by `open_manifest`. The only producer of a `Some(..)` is the REDEEM path
 *  (`fulfil.rs`'s `carrier4_served_by`), whose result Chat discarded. So the provenance branches
 *  below were unreachable in production while carrying two green tests. Extracting the copy here
 *  lets Chat surface it at the point of redemption without holding the manifest tree, keeping the
 *  "one hand-off, not two" design that made Chat discard the result in the first place.
 *
 *  Pure on purpose: `servingPeerName` is injected so this needs neither a store nor a component,
 *  and both callers pass their own resolver. */
export function importToast(
	result: ImportedManifest,
	servingPeerName: (npub: string) => string,
): { text: string; kind: 'success' | 'error' } {
	const reServed = result.served_by !== undefined;
	if (result.stale && reServed) {
		return {
			text: `Imported an older copy from ${servingPeerName(result.served_by!)}'s cache — the owner is offline, so ask again once they're back.`,
			kind: 'error',
		};
	}
	if (result.stale) {
		return { text: 'Imported an older version of this list. Ask the owner for a fresh manifest.', kind: 'error' };
	}
	if (reServed) {
		return { text: `Full manifest imported from ${servingPeerName(result.served_by!)}'s cached copy`, kind: 'success' };
	}
	return { text: 'Full manifest imported', kind: 'success' };
}
