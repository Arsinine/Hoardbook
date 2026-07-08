// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import ConfirmButton from './ConfirmButton.svelte';

afterEach(cleanup);

describe('ConfirmButton — inline two-step confirm (mirrors the Settings wipe-data pattern)', () => {
	it('two_click_confirm_matches_wipe_pattern', async () => {
		const confirms: number[] = [];
		const { getByRole, queryByText } = render(ConfirmButton, {
			props: { label: 'Remove', onconfirm: () => confirms.push(1) },
		});

		// First render: just the trigger, no "are you sure" prompt yet.
		const trigger = getByRole('button', { name: 'Remove' });
		expect(queryByText(/are you sure/i)).toBeNull();

		// First click reveals the confirm prompt — does NOT fire confirm yet.
		await fireEvent.click(trigger);
		expect(confirms.length).toBe(0);
		expect(queryByText(/are you sure/i)).not.toBeNull();

		// Second click (Confirm) fires exactly once.
		const confirmBtn = getByRole('button', { name: /confirm/i });
		await fireEvent.click(confirmBtn);
		expect(confirms.length).toBe(1);
	});

	it('cancel collapses the confirm prompt without firing', async () => {
		const confirms: number[] = [];
		const { getByRole, queryByText } = render(ConfirmButton, {
			props: { label: 'Remove', onconfirm: () => confirms.push(1) },
		});

		await fireEvent.click(getByRole('button', { name: 'Remove' }));
		expect(queryByText(/are you sure/i)).not.toBeNull();

		await fireEvent.click(getByRole('button', { name: /cancel/i }));
		expect(confirms.length).toBe(0);
		expect(queryByText(/are you sure/i)).toBeNull();
		expect(getByRole('button', { name: 'Remove' })).toBeTruthy();
	});
});
