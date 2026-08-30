// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import CollectionRow from './CollectionRow.svelte';
import type { Collection } from '../types.js';

afterEach(cleanup);

function makeCollection(overrides: Partial<Collection> = {}): Collection {
	return {
		slug: 'movies',
		path_alias: 'Movies',
		item_count: 3,
		total_bytes: 1000,
		content_types: ['video'],
		tags: [],
		languages: [],
		last_updated: '2026-01-01T00:00:00Z',
		listing: [],
		published: false,
		...overrides,
	};
}

/** Open the row's [⋯] overflow menu (the trigger carries the "Collection actions" aria-label). */
async function openMenu(container: HTMLElement) {
	const btn = container.querySelector<HTMLButtonElement>('[aria-label="Collection actions"]');
	if (!btn) throw new Error('overflow-menu trigger not found');
	await fireEvent.click(btn);
}

describe('CollectionRow', () => {
	it('draft_row_shows_publish_menu_item', async () => {
		const { container, findByRole, queryByRole } = render(CollectionRow, {
			props: { collection: makeCollection({ published: false }) },
		});
		await openMenu(container);
		expect(await findByRole('menuitem', { name: /^publish$/i })).toBeTruthy();
		expect(queryByRole('menuitem', { name: /^unpublish$/i })).toBeNull();
	});

	it('published_row_shows_unpublish_menu_item', async () => {
		const { container, findByRole, queryByRole } = render(CollectionRow, {
			props: { collection: makeCollection({ published: true }) },
		});
		await openMenu(container);
		expect(await findByRole('menuitem', { name: /^unpublish$/i })).toBeTruthy();
		expect(queryByRole('menuitem', { name: /^publish$/i })).toBeNull();
	});

	it('q138_no_export_affordance_in_the_collections_menu', async () => {
		// QURATOR-138 (owner 2026-08-27): "Delete the … export buttons in collections as well."
		// The OverflowMenu entry (and its text/markdown/manifest submenu) is the collections-list
		// export affordance — it must be GONE. `onexport` is removed from CollectionRow's Props
		// entirely; nothing else in the tree passes it to this component.
		const { container, findByRole, queryByRole } = render(CollectionRow, {
			props: { collection: makeCollection() },
		});

		await openMenu(container);
		// The other actions are still there (this is a deletion of export, not of the menu).
		expect(await findByRole('menuitem', { name: /^rescan$/i })).toBeTruthy();
		expect(await findByRole('menuitem', { name: /^edit details$/i })).toBeTruthy();
		// THE assertion: no Export item, none of its submenu formats.
		expect(queryByRole('menuitem', { name: /^export$/i })).toBeNull();
		expect(queryByRole('menuitem', { name: /plain text/i })).toBeNull();
		expect(queryByRole('menuitem', { name: /markdown/i })).toBeNull();
		expect(queryByRole('menuitem', { name: /manifest/i })).toBeNull();
	});

	it('remove_requires_confirm', async () => {
		const removed = vi.fn();
		const { container, getByRole, findByRole } = render(CollectionRow, {
			props: { collection: makeCollection(), onremove: removed },
		});

		await openMenu(container);
		await fireEvent.click(await findByRole('menuitem', { name: /^remove$/i }));
		// First click only reveals the confirm prompt.
		expect(removed).not.toHaveBeenCalled();

		await fireEvent.click(getByRole('button', { name: /confirm/i }));
		expect(removed).toHaveBeenCalledTimes(1);
	});
});
