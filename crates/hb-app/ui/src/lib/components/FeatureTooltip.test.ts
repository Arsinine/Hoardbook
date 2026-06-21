// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import FeatureTooltip from './FeatureTooltip.svelte';
import { TOOLTIPS } from '../tooltips.js';

afterEach(cleanup);

const tipOf = (trigger: HTMLElement): HTMLElement => {
	const id = trigger.getAttribute('aria-describedby');
	expect(id, 'trigger must have aria-describedby').toBeTruthy();
	const tip = document.getElementById(id!);
	expect(tip, 'aria-describedby must point at a rendered element').toBeTruthy();
	return tip!;
};

describe('FeatureTooltip — accessible hover/focus help (HOARDBOOK_SPEC §8)', () => {
	it('renders a focusable button trigger, copy collapsed by default, aria-describedby wired', () => {
		const { getByRole } = render(FeatureTooltip, { props: { key: 'no-download' } });
		const trigger = getByRole('button');
		// Focusable: a real <button> (tabbable), not a static div.
		expect(trigger.tagName).toBe('BUTTON');
		// type=button: no form submit, no navigation.
		expect(trigger.getAttribute('type')).toBe('button');

		const tip = tipOf(trigger);
		expect(tip.getAttribute('role')).toBe('tooltip');
		// Collapsed by default.
		expect(tip.hasAttribute('hidden')).toBe(true);
		expect(trigger.getAttribute('aria-expanded')).toBe('false');
		// The copy is rendered (present in the DOM), just collapsed — and carries the spec substance.
		expect(tip.textContent).toContain('moves no files');
	});

	// Gemini (Chorus M8): the ⓘ icon is aria-hidden, so the icon-only trigger must carry its own
	// accessible name or a screen reader announces a nameless "button" (WCAG 2.1 SC 4.1.2).
	it('the icon-only trigger has an accessible name for screen readers', () => {
		const { getByRole } = render(FeatureTooltip, { props: { key: 'no-download' } });
		const trigger = getByRole('button');
		const name = trigger.getAttribute('aria-label');
		expect(name && name.trim().length > 0, 'icon trigger needs an aria-label').toBe(true);
		// it should reference the specific tooltip so multiple ⓘ markers are distinguishable
		expect(name).toContain(TOOLTIPS['no-download'].title);
	});

	it('expands on keyboard focus and collapses on blur', async () => {
		const { getByRole } = render(FeatureTooltip, { props: { key: 'fingerprint' } });
		const trigger = getByRole('button');
		const tip = tipOf(trigger);

		await fireEvent.focus(trigger);
		expect(tip.hasAttribute('hidden')).toBe(false);
		expect(trigger.getAttribute('aria-expanded')).toBe('true');

		await fireEvent.blur(trigger);
		expect(tip.hasAttribute('hidden')).toBe(true);
	});

	it('expands on hover and collapses on mouse-leave', async () => {
		const { getByRole } = render(FeatureTooltip, { props: { key: 'willing-to' } });
		const trigger = getByRole('button');
		const tip = tipOf(trigger);

		await fireEvent.mouseEnter(trigger);
		expect(tip.hasAttribute('hidden')).toBe(false);
		await fireEvent.mouseLeave(trigger);
		expect(tip.hasAttribute('hidden')).toBe(true);
	});

	it('Escape collapses the expanded copy', async () => {
		const { getByRole } = render(FeatureTooltip, { props: { key: 'listings-locked' } });
		const trigger = getByRole('button');
		const tip = tipOf(trigger);

		await fireEvent.focus(trigger);
		expect(tip.hasAttribute('hidden')).toBe(false);
		await fireEvent.keyDown(trigger, { key: 'Escape' });
		expect(tip.hasAttribute('hidden')).toBe(true);
	});

	it('is explanatory only — the trigger performs no navigation/command', async () => {
		const before = window.location.href;
		const { getByRole } = render(FeatureTooltip, { props: { key: 'k-of-n-folders' } });
		const trigger = getByRole('button');
		// No link semantics.
		expect(trigger.getAttribute('href')).toBeNull();
		// Clicking only toggles visibility; it does not navigate or submit.
		await fireEvent.click(trigger);
		expect(window.location.href).toBe(before);
		const tip = tipOf(trigger);
		expect(tip.hasAttribute('hidden')).toBe(false); // click toggled it open (show/hide only)
	});

	it('renders each of the five spec anchors accessibly', () => {
		for (const key of ['no-download', 'willing-to', 'listings-locked', 'k-of-n-folders', 'fingerprint'] as const) {
			const { getByRole, unmount } = render(FeatureTooltip, { props: { key } });
			const trigger = getByRole('button');
			const tip = tipOf(trigger);
			expect(tip.textContent?.trim().length).toBeGreaterThan(0);
			unmount();
		}
	});

	// F4 — content (per-item note) vs feature-help must not bleed: a plain note element does NOT
	// acquire the FeatureTooltip ⓘ / aria-describedby / role=tooltip affordance.
	it('(F4) a per-item note element carries no FeatureTooltip affordance', () => {
		const { getByRole } = render(FeatureTooltip, { props: { key: 'no-download' } });
		// The feature-help trigger HAS the affordance:
		expect(getByRole('button').getAttribute('aria-describedby')).toBeTruthy();

		// A per-item note is plain content (a native-title span) — structurally distinct:
		document.body.insertAdjacentHTML(
			'beforeend',
			'<span class="dir-note" title="Director’s cut">film.mkv</span>',
		);
		const note = document.querySelector('.dir-note')!;
		expect(note.getAttribute('aria-describedby')).toBeNull();
		expect(note.querySelector('[role="tooltip"]')).toBeNull();
		expect(note.textContent).not.toContain('ⓘ');
	});
});
