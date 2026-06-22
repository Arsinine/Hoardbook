// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import VisibilitySelector from './VisibilitySelector.svelte';

afterEach(cleanup);

describe('VisibilitySelector (M10)', () => {
	it('defaults to Public and shows no not-DRM note', () => {
		const { getByLabelText, queryByRole } = render(VisibilitySelector);
		const select = getByLabelText('Visibility') as HTMLSelectElement;
		expect(select.value).toBe('Public');
		expect(queryByRole('note')).toBeNull();
	});

	it('reflects an explicit Private prop and shows the honest not-DRM note', () => {
		const { getByLabelText, getByRole } = render(VisibilitySelector, {
			props: { visibility: 'Private' },
		});
		expect((getByLabelText('Visibility') as HTMLSelectElement).value).toBe('Private');
		const note = getByRole('note');
		expect(note.textContent?.toLowerCase()).toContain('not drm');
		expect(note.textContent?.toLowerCase()).toContain('future republishes');
	});

	it('dispatches a change event and reveals the note when switched to Private', async () => {
		const { getByLabelText, component, queryByRole } = render(VisibilitySelector);
		const changes: string[] = [];
		component.$on('change', (e: CustomEvent<string>) => changes.push(e.detail));

		const select = getByLabelText('Visibility') as HTMLSelectElement;
		expect(queryByRole('note')).toBeNull();
		await fireEvent.change(select, { target: { value: 'Private' } });

		expect(changes).toEqual(['Private']);
		expect(queryByRole('note')).not.toBeNull(); // not-DRM note now visible
	});
});
