// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import AddContactDialog from './AddContactDialog.svelte';
import type { Group } from '../types.js';

afterEach(cleanup);

const GROUPS: Group[] = [{ name: 'Inner Circle', pubkeys: [], trusted: true }];

describe('AddContactDialog (M13 W5 Slice 2)', () => {
	it('petname_prefilled_with_display_name_and_editable', async () => {
		const { getByLabelText } = render(AddContactDialog, {
			props: { open: true, displayName: 'Alice', groups: GROUPS },
		});
		const input = getByLabelText(/petname/i) as HTMLInputElement;
		expect(input.value).toBe('Alice');

		await fireEvent.input(input, { target: { value: 'Al' } });
		expect(input.value).toBe('Al');
	});

	it('save_emits_petname_and_group', async () => {
		const { getByLabelText, getByRole, component } = render(AddContactDialog, {
			props: { open: true, displayName: 'Alice', groups: GROUPS },
		});
		const saved = vi.fn();
		component.$on('save', saved);

		await fireEvent.input(getByLabelText(/petname/i), { target: { value: 'Al' } });
		await fireEvent.change(getByRole('combobox'), { target: { value: 'Inner Circle' } });
		await fireEvent.click(getByRole('button', { name: /add contact/i }));

		expect(saved).toHaveBeenCalledTimes(1);
		expect(saved.mock.calls[0][0].detail).toEqual({ petname: 'Al', group: 'Inner Circle' });
	});

	it('skip_emits_skip_without_petname', async () => {
		const { getByRole, component } = render(AddContactDialog, {
			props: { open: true, displayName: 'Alice', groups: GROUPS },
		});
		const saved = vi.fn();
		const skipped = vi.fn();
		component.$on('save', saved);
		component.$on('skip', skipped);

		await fireEvent.click(getByRole('button', { name: /skip/i }));

		expect(skipped).toHaveBeenCalledTimes(1);
		expect(skipped.mock.calls[0][0].detail).toBeNull();
		expect(saved).not.toHaveBeenCalled();
	});
});
