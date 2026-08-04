// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import CreateGroupDialog from './CreateGroupDialog.svelte';

afterEach(cleanup);

describe('CreateGroupDialog (M13 W5 Slice 2)', () => {
	it('renders_even_with_zero_existing_groups', () => {
		// The whole point: today every group picker is gated behind `groups.length > 0`, making
		// group creation an unreachable dead path for a first-time user.
		const { getByRole, getByLabelText } = render(CreateGroupDialog, { props: { open: true } });
		expect(getByLabelText(/name/i)).toBeTruthy();
		expect(getByRole('button', { name: /create/i })).toBeTruthy();
	});

	it('emits_create_with_name_color', async () => {
		const creates: { name: string; color: string }[] = [];
		const { getByLabelText, getByRole } = render(CreateGroupDialog, {
			props: { open: true, oncreate: (detail) => creates.push(detail) },
		});

		await fireEvent.input(getByLabelText(/name/i), { target: { value: 'Inner Circle' } });
		const swatches = document.querySelectorAll('.swatch');
		expect(swatches.length).toBeGreaterThan(0);
		await fireEvent.click(swatches[1]);
		await fireEvent.click(getByRole('button', { name: /create/i }));

		expect(creates.length).toBe(1);
		expect(creates[0].name).toBe('Inner Circle');
		expect(creates[0].color).toMatch(/^#/);
	});
});
