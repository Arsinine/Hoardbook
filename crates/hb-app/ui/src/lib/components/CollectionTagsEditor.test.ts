// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import CollectionTagsEditor from './CollectionTagsEditor.svelte';

afterEach(cleanup);

// `tags` is a $bindable prop with no separate change event (M14) — read the rendered chip list
// back out of the DOM to assert the resulting array, since there's no `component.$on` in Svelte 5.
function chipLabels(container: HTMLElement): string[] {
	return Array.from(container.querySelectorAll('.chip')).map(
		(chip) => chip.childNodes[0].textContent?.trim() ?? '',
	);
}

describe('CollectionTagsEditor — chip tag input', () => {
	it('enter_and_comma_add_tag_lowercased', async () => {
		const { getByPlaceholderText, getByText, container } = render(CollectionTagsEditor, {
			props: { tags: [] },
		});

		const input = getByPlaceholderText(/add a tag/i) as HTMLInputElement;
		await fireEvent.input(input, { target: { value: 'ANIME' } });
		await fireEvent.keyDown(input, { key: 'Enter' });
		expect(getByText('anime')).toBeTruthy();
		expect(chipLabels(container)).toEqual(['anime']);

		await fireEvent.input(input, { target: { value: 'SciFi,' } });
		await fireEvent.keyDown(input, { key: ',' });
		expect(getByText('scifi')).toBeTruthy();
		expect(chipLabels(container)).toEqual(['anime', 'scifi']);
	});

	it('backspace_removes_last', async () => {
		const { getByPlaceholderText, queryByText, container } = render(CollectionTagsEditor, {
			props: { tags: ['anime', 'scifi'] },
		});

		const input = getByPlaceholderText(/add a tag/i) as HTMLInputElement;
		// Input is empty, so Backspace pops the last tag.
		await fireEvent.keyDown(input, { key: 'Backspace' });
		expect(queryByText('scifi')).toBeNull();
		expect(chipLabels(container)).toEqual(['anime']);
	});

	it('removing_a_chip_leaves_the_rest', async () => {
		const { getByText, container } = render(CollectionTagsEditor, {
			props: { tags: ['anime', 'scifi'] },
		});

		// Removing a specific chip via its × leaves the rest of the array intact.
		const removeBtn = getByText('anime').querySelector('button') as HTMLButtonElement;
		await fireEvent.click(removeBtn);
		expect(chipLabels(container)).toEqual(['scifi']);
	});
});
