// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import CollectionTagsEditor from './CollectionTagsEditor.svelte';

afterEach(cleanup);

describe('CollectionTagsEditor — chip tag input', () => {
	it('enter_and_comma_add_tag_lowercased', async () => {
		const { getByPlaceholderText, getByText, component } = render(CollectionTagsEditor, {
			props: { tags: [] },
		});
		const changes: string[][] = [];
		component.$on('change', (e: CustomEvent<string[]>) => changes.push(e.detail));

		const input = getByPlaceholderText(/add a tag/i) as HTMLInputElement;
		await fireEvent.input(input, { target: { value: 'ANIME' } });
		await fireEvent.keyDown(input, { key: 'Enter' });
		expect(getByText('anime')).toBeTruthy();
		expect(changes.at(-1)).toEqual(['anime']);

		await fireEvent.input(input, { target: { value: 'SciFi,' } });
		await fireEvent.keyDown(input, { key: ',' });
		expect(getByText('scifi')).toBeTruthy();
		expect(changes.at(-1)).toEqual(['anime', 'scifi']);
	});

	it('backspace_removes_last', async () => {
		const { getByPlaceholderText, queryByText, component } = render(CollectionTagsEditor, {
			props: { tags: ['anime', 'scifi'] },
		});
		const changes: string[][] = [];
		component.$on('change', (e: CustomEvent<string[]>) => changes.push(e.detail));

		const input = getByPlaceholderText(/add a tag/i) as HTMLInputElement;
		// Input is empty, so Backspace pops the last tag.
		await fireEvent.keyDown(input, { key: 'Backspace' });
		expect(queryByText('scifi')).toBeNull();
		expect(changes.at(-1)).toEqual(['anime']);
	});

	it('dispatches_change_with_tag_array', async () => {
		const { getByText, component } = render(CollectionTagsEditor, {
			props: { tags: ['anime', 'scifi'] },
		});
		const changes: string[][] = [];
		component.$on('change', (e: CustomEvent<string[]>) => changes.push(e.detail));

		// Removing a specific chip via its × also dispatches the full resulting array.
		const removeBtn = getByText('anime').querySelector('button') as HTMLButtonElement;
		await fireEvent.click(removeBtn);
		expect(changes.at(-1)).toEqual(['scifi']);
	});
});
