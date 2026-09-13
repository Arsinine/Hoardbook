// @vitest-environment jsdom
// QURATOR-206 — the Add-collection wizard is ONE two-column screen: Directory (left) + Details
// (right), one header, one Cancel/Publish footer, no step state. "Start scan" fills the scan
// summary; it never swaps modals. The same screen opens pre-loaded for a row menu's
// Rescan / Edit details.
//
// ⚠ P-13: jsdom computes no layout. These tests CANNOT prove the side-by-side arrangement or the
// ~720px single-column collapse — both are CSS-only facts. What they pin is that both regions are
// in the DOM at once (no step gating), that Publish is gated, and that the reopen path pre-loads.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import AddCollectionModal from './AddCollectionModal.svelte';
import type { Collection } from '../types.js';

vi.mock('../api.js', () => ({
	scanDirectory: vi.fn(),
	listSubdirs: vi.fn().mockResolvedValue([]),
	updateCollectionMeta: vi.fn().mockResolvedValue(undefined),
	updateCollectionVisibility: vi.fn().mockResolvedValue(undefined),
	publishCollection: vi.fn().mockResolvedValue(undefined),
}));

import {
	scanDirectory,
	listSubdirs,
	updateCollectionMeta,
	updateCollectionVisibility,
	publishCollection
} from '../api.js';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

function makeCollection(overrides: Partial<Collection> = {}): Collection {
	return {
		slug: 'movies',
		path_alias: 'Movies',
		item_count: 3,
		total_bytes: 1000,
		content_types: [],
		tags: [],
		languages: [],
		last_updated: '2026-01-01T00:00:00Z',
		listing: [],
		published: false,
		...overrides,
	};
}

async function runScan(getByPlaceholderText: (id: RegExp) => HTMLElement, getByRole: (role: string, opts?: { name?: RegExp }) => HTMLElement) {
	await fireEvent.input(getByPlaceholderText(/mnt\/data/i), { target: { value: '/mnt/movies' } });
	await fireEvent.input(getByPlaceholderText(/criterion collection/i), { target: { value: 'Movies' } });
	await fireEvent.click(getByRole('button', { name: /start scan/i }));
}

describe('AddCollectionModal (QURATOR-206 — one two-column screen)', () => {
	it('both_columns_mount_together_and_a_scan_does_not_swap_modals', async () => {
		const scanned = makeCollection({ slug: 'scanned-slug', path_alias: 'Scanned Folder' });
		(scanDirectory as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(scanned);

		const { getByPlaceholderText, getByRole, getByText, findByText, container } = render(AddCollectionModal, {
			props: { open: true },
		});

		// The Details region is present BEFORE any scan — the old wizard showed it only after the
		// step-1→2 swap (the devtest #3 complaint).
		expect(getByText(/content types/i)).toBeTruthy();
		expect(getByPlaceholderText(/mnt\/data/i)).toBeTruthy();

		await runScan(getByPlaceholderText, getByRole);

		// The scan fills the summary and the same screen stays up: still one dialog, both regions
		// still mounted, and the scan verb relabels to Rescan on the same button.
		expect(await findByText(/Scanned — 3 files/)).toBeTruthy();
		expect(container.querySelectorAll('[role="dialog"]').length).toBe(1);
		expect(getByText(/content types/i)).toBeTruthy();
		expect(scanDirectory).toHaveBeenCalled();
		expect(getByRole('button', { name: /^rescan$/i })).toBeTruthy();
	});

	it('publish_stays_disabled_until_both_a_scan_exists_and_a_content_type_is_picked', async () => {
		const scanned = makeCollection({ slug: 'scanned-slug' });
		(scanDirectory as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(scanned);

		const { getByPlaceholderText, getByRole } = render(AddCollectionModal, {
			props: { open: true },
		});

		// Unscanned AND typeless: disabled. Picking a content type alone is not enough in the Add
		// flow — there is nothing to publish until a scan (the cache handle QURATOR-207 promotes).
		let publishBtn = getByRole('button', { name: /^publish$/i }) as HTMLButtonElement;
		expect(publishBtn.disabled).toBe(true);
		await fireEvent.click(getByRole('button', { name: 'Video' }));
		expect(publishBtn.disabled).toBe(true);

		await runScan(getByPlaceholderText, getByRole);
		await waitFor(() => expect(publishBtn.disabled).toBe(false));
	});

	it('publish_disabled_until_a_content_type_is_picked_on_reopen', async () => {
		const col = makeCollection({ content_types: [] });
		const { getByRole } = render(AddCollectionModal, {
			props: { open: true, editCollection: col },
		});

		const publishBtn = getByRole('button', { name: /^publish$/i }) as HTMLButtonElement;
		expect(publishBtn.disabled).toBe(true);

		await fireEvent.click(getByRole('button', { name: 'Video' }));
		expect(publishBtn.disabled).toBe(false);
	});

	it('reopen_opens_preloaded_with_the_tree_preresolved', async () => {
		const col = makeCollection({
			path_alias: 'Movies',
			description: 'Existing notes',
			content_types: ['video'],
		});

		const { getByRole, getByText, findByDisplayValue } = render(AddCollectionModal, {
			props: { open: true, editCollection: col, initialPath: '/mnt/existing' },
		});

		// Header says Edit, and the tree was pre-resolved from the caller-supplied root path.
		expect(getByText(/edit collection/i)).toBeTruthy();
		await waitFor(() => expect(listSubdirs).toHaveBeenCalledWith('/mnt/existing'));

		// Both columns seeded from the existing collection: display name, notes, content type,
		// and the scan summary line reflecting the stored counts.
		expect(await findByDisplayValue('Movies')).toBeTruthy();
		expect(await findByDisplayValue('Existing notes')).toBeTruthy();
		expect(getByRole('button', { name: 'Video' }).className).toContain('ct-on');
		expect(getByText(/Scanned — 3 files/)).toBeTruthy();
		expect(getByRole('button', { name: /^rescan$/i })).toBeTruthy();
	});

	it('q138_save_draft_button_is_gone_and_details_still_publish', async () => {
		// QURATOR-138: "Delete the 'Save Draft' button … in edit details." The footer has
		// exactly Cancel + Publish; there is no third path, and Publish still persists-then-publishes.
		const col = makeCollection({ content_types: ['video'] });
		const closed = vi.fn();
		const { getByRole, queryByRole } = render(AddCollectionModal, {
			props: { open: true, editCollection: col, onclose: closed },
		});

		expect(queryByRole('button', { name: /save draft/i })).toBeNull();
		expect(getByRole('button', { name: /^cancel$/i })).toBeTruthy();

		await fireEvent.click(getByRole('button', { name: /^publish$/i }));
		await waitFor(() => expect(closed).toHaveBeenCalled());
		expect(updateCollectionMeta).toHaveBeenCalled();
		expect(publishCollection).toHaveBeenCalled();
	});

	it('q207_cancel_after_a_scan_writes_nothing_durable', async () => {
		// QURATOR-207: the scan is an in-memory cache handle; Cancel discards by construction.
		// Neither the meta write nor the promote-at-Publish may have happened.
		const scanned = makeCollection({ slug: 'scanned-slug' });
		(scanDirectory as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(scanned);
		const closed = vi.fn();

		const { getByPlaceholderText, getByRole, findByText } = render(AddCollectionModal, {
			props: { open: true, onclose: closed },
		});

		await runScan(getByPlaceholderText, getByRole);
		expect(await findByText(/Scanned — 3 files/)).toBeTruthy();

		await fireEvent.click(getByRole('button', { name: /^cancel$/i }));
		await waitFor(() => expect(closed).toHaveBeenCalled());
		expect(updateCollectionMeta).not.toHaveBeenCalled();
		expect(updateCollectionVisibility).not.toHaveBeenCalled();
		expect(publishCollection).not.toHaveBeenCalled();
	});

	// QURATOR-97 — close() wipes all run state, so a stray backdrop click silently discards the
	// run. Backdrop must not close; Cancel stays.
	it('q97_backdrop_click_does_not_close_the_screen', async () => {
		const col = makeCollection({ content_types: ['video'] });
		const closed = vi.fn();
		const { container, getByRole } = render(AddCollectionModal, {
			props: { open: true, editCollection: col, onclose: closed },
		});

		const backdrop = container.querySelector('.modal-backdrop') as HTMLElement;
		expect(backdrop).toBeTruthy();
		await fireEvent.click(backdrop);

		expect(closed).not.toHaveBeenCalled();
		expect(getByRole('dialog')).toBeTruthy();
	});
});
