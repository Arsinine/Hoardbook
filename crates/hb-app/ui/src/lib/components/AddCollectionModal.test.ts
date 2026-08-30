// @vitest-environment jsdom
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

import { scanDirectory, updateCollectionMeta, publishCollection } from '../api.js';

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

describe('AddCollectionModal', () => {
	it('step1_scan_advances_to_step2_details', async () => {
		const scanned = makeCollection({ slug: 'scanned-slug', path_alias: 'Scanned Folder' });
		(scanDirectory as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(scanned);

		const { getByPlaceholderText, getByRole, findByText } = render(AddCollectionModal, {
			props: { open: true },
		});

		await fireEvent.input(getByPlaceholderText(/mnt\/data/i), { target: { value: '/mnt/movies' } });
		await fireEvent.input(getByPlaceholderText(/criterion collection/i), { target: { value: 'Movies' } });
		await fireEvent.click(getByRole('button', { name: /start scan/i }));

		await findByText(/content types/i);
		expect(scanDirectory).toHaveBeenCalled();
		// Step 2 shows the scanned collection's name in its header.
		expect(await findByText('Scanned Folder')).toBeTruthy();
	});

	it('publish_disabled_until_a_content_type_is_selected', async () => {
		const col = makeCollection({ content_types: [] });
		const { getByRole } = render(AddCollectionModal, {
			props: { open: true, editCollection: col },
		});

		const publishBtn = getByRole('button', { name: /^publish$/i }) as HTMLButtonElement;
		expect(publishBtn.disabled).toBe(true);

		await fireEvent.click(getByRole('button', { name: 'Video' }));
		expect(publishBtn.disabled).toBe(false);
	});

	it('q138_save_draft_button_is_gone_and_details_still_publish', async () => {
		// QURATOR-138: "Delete the 'Save Draft' button … in edit details." The footer now has
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

	// QURATOR-97 — the wizard's close() resets step/collection, so a stray backdrop click on the
	// step-2 details modal silently discards the run. Backdrop must not close; Cancel stays.
	it('q97_backdrop_click_does_not_close_the_details_step', async () => {
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
