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

	it('export_menu_opens_from_overflow_menu', async () => {
		const { container, findByRole, component } = render(CollectionRow, {
			props: { collection: makeCollection() },
		});
		const exported = vi.fn();
		component.$on('export', exported);

		await openMenu(container);
		await fireEvent.click(await findByRole('menuitem', { name: /^export$/i }));
		const plainText = await findByRole('menuitem', { name: /plain text/i });
		expect(plainText).toBeTruthy();

		await fireEvent.click(plainText);
		expect(exported).toHaveBeenCalledTimes(1);
		expect(exported.mock.calls[0][0].detail).toEqual({ slug: 'movies', format: 'text' });
	});

	it('remove_requires_confirm', async () => {
		const { container, getByRole, findByRole, component } = render(CollectionRow, {
			props: { collection: makeCollection() },
		});
		const removed = vi.fn();
		component.$on('remove', removed);

		await openMenu(container);
		await fireEvent.click(await findByRole('menuitem', { name: /^remove$/i }));
		// First click only reveals the confirm prompt.
		expect(removed).not.toHaveBeenCalled();

		await fireEvent.click(getByRole('button', { name: /confirm/i }));
		expect(removed).toHaveBeenCalledTimes(1);
	});
});
