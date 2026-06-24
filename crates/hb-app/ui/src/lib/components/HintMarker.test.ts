// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import HintMarker from './HintMarker.svelte';

afterEach(cleanup);

const tipOf = (trigger: HTMLElement): HTMLElement => {
	const id = trigger.getAttribute('aria-describedby');
	expect(id, 'trigger must have aria-describedby').toBeTruthy();
	const tip = document.getElementById(id!);
	expect(tip, 'aria-describedby must point at a rendered element').toBeTruthy();
	return tip!;
};

describe('HintMarker — Windows-style "?" field-hint marker', () => {
	it('renders a focusable "?" button, hint collapsed by default, aria wired', () => {
		const { getByRole } = render(HintMarker, { props: { text: 'fill in for local meetups', label: 'Region / City' } });
		const trigger = getByRole('button');
		expect(trigger.tagName).toBe('BUTTON');
		expect(trigger.getAttribute('type')).toBe('button');
		expect(trigger.textContent).toContain('?');
		// The icon-only trigger carries its own accessible name referencing the field.
		expect(trigger.getAttribute('aria-label')).toContain('Region / City');

		const tip = tipOf(trigger);
		expect(tip.getAttribute('role')).toBe('tooltip');
		expect(tip.hasAttribute('hidden')).toBe(true);
		expect(trigger.getAttribute('aria-expanded')).toBe('false');
		expect(tip.textContent).toContain('fill in for local meetups');
	});

	it('expands on hover/focus and collapses on leave/blur/Escape', async () => {
		const { getByRole } = render(HintMarker, { props: { text: 'hint', label: 'X' } });
		const trigger = getByRole('button');
		const tip = tipOf(trigger);

		await fireEvent.mouseEnter(trigger);
		expect(tip.hasAttribute('hidden')).toBe(false);
		await fireEvent.mouseLeave(trigger);
		expect(tip.hasAttribute('hidden')).toBe(true);

		await fireEvent.focus(trigger);
		expect(tip.hasAttribute('hidden')).toBe(false);
		await fireEvent.keyDown(trigger, { key: 'Escape' });
		expect(tip.hasAttribute('hidden')).toBe(true);
	});

	it('is explanatory only — clicking toggles visibility, never navigates', async () => {
		const before = window.location.href;
		const { getByRole } = render(HintMarker, { props: { text: 'hint', label: 'X' } });
		const trigger = getByRole('button');
		expect(trigger.getAttribute('href')).toBeNull();
		await fireEvent.click(trigger);
		expect(window.location.href).toBe(before);
		expect(tipOf(trigger).hasAttribute('hidden')).toBe(false);
	});
});
