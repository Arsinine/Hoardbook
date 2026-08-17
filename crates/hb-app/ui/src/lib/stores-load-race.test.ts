// Concern-1 — loadCollectionsInto/loadContactsInto lacked a latest-request guard. Overlapping loads
// (retry + poll + mutation refresh) applied whichever result settled LAST, not whichever was
// requested last: a slow retry landing after a fast poll's newer success could silently overwrite
// good data with stale data, or a slow FAILURE landing after a fast newer SUCCESS could re-raise an
// error flag the newer load had already cleared.
//
// Behavioural pin: start load A (slow, will eventually REJECT), then — before A settles — start load
// B (fast, RESOLVES). B lands first. When A's rejection finally lands, it must be a no-op: the store
// still holds B's data and the error flag must still read false.
import { describe, expect, it, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
	collections,
	collectionsLoadError,
	contacts,
	contactsLoadError,
	loadCollectionsInto,
	loadContactsInto,
} from './stores.js';
import type { Collection, CachedPeer } from './types.js';

afterEach(() => {
	collections.set([]);
	collectionsLoadError.set(false);
	contacts.set([]);
	contactsLoadError.set(false);
});

function deferred<T>() {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
	return { promise, resolve, reject };
}

const COLLECTION_B: Collection = {
	slug: 'b', path_alias: 'B — the newer load', description: '', tags: [], languages: [],
	item_count: 0, total_bytes: 0, content_types: [], last_updated: '2026-08-17T00:00:00Z', listing: [],
};

const PEER_B: CachedPeer = {
	npub: 'npub1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
	collections: [], online: false, last_fetched: '2026-08-17T00:00:00Z', local_tags: [],
} as CachedPeer;

describe('Concern-1 — loadCollectionsInto: a superseded load cannot clobber a newer one', () => {
	it('slow A rejects AFTER fast B resolves → final state is B, error flag stays clear', async () => {
		const a = deferred<Collection[]>();
		const b = deferred<Collection[]>();

		const pA = loadCollectionsInto(() => a.promise); // starts first, will fail — slow
		const pB = loadCollectionsInto(() => b.promise); // starts second, succeeds — fast

		b.resolve([COLLECTION_B]);
		await pB;
		expect(get(collections)).toEqual([COLLECTION_B]);
		expect(get(collectionsLoadError)).toBe(false);

		// A's failure lands LAST — it must be a no-op against B's already-applied success.
		a.reject(new Error('stale relay timeout'));
		await pA;
		expect(get(collections)).toEqual([COLLECTION_B]); // untouched by the stale rejection
		expect(get(collectionsLoadError)).toBe(false); // NOT re-raised by the superseded failure
	});
});

describe('Concern-1 — loadContactsInto: a superseded load cannot clobber a newer one', () => {
	it('slow A rejects AFTER fast B resolves → final state is B, error flag stays clear', async () => {
		const a = deferred<CachedPeer[]>();
		const b = deferred<CachedPeer[]>();

		const pA = loadContactsInto(() => a.promise); // starts first, will fail — slow
		const pB = loadContactsInto(() => b.promise); // starts second, succeeds — fast

		b.resolve([PEER_B]);
		const resultB = await pB;
		expect(resultB).toEqual([PEER_B]);
		expect(get(contacts)).toEqual([PEER_B]);
		expect(get(contactsLoadError)).toBe(false);

		// A's failure lands LAST — it must be a no-op against B's already-applied success.
		a.reject(new Error('stale relay timeout'));
		const resultA = await pA;
		expect(resultA).toBeNull(); // superseded — the caller gets nothing, not a stale error
		expect(get(contacts)).toEqual([PEER_B]); // untouched by the stale rejection
		expect(get(contactsLoadError)).toBe(false); // NOT re-raised by the superseded failure
	});
});
