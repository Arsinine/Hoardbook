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

	it('save_draft_closes_without_publishing', async () => {
		const col = makeCollection({ content_types: ['video'] });
		const { getByRole, component } = render(AddCollectionModal, {
			props: { open: true, editCollection: col },
		});

		const closed = vi.fn();
		component.$on('close', closed);

		await fireEvent.click(getByRole('button', { name: /save draft/i }));
		await waitFor(() => expect(closed).toHaveBeenCalled());

		expect(updateCollectionMeta).toHaveBeenCalled();
		expect(publishCollection).not.toHaveBeenCalled();
	});
});
