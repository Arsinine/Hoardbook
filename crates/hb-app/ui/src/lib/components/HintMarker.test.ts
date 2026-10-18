// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
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

// jsdom computes no layout, so every getBoundingClientRect returns all zeros. QURATOR-383's
// placement maths reads the TRIGGER rect — stub it per element. (Rendered geometry itself is a
// layout fact jsdom cannot prove; these tests pin the placement DECISION, not pixels.)
const stubRect = (el: Element, rect: Partial<DOMRect>) => {
	Object.defineProperty(el, 'getBoundingClientRect', {
		value: () =>
			({
				x: 0,
				y: 0,
				width: 14,
				height: 18,
				top: 0,
				left: 0,
				right: 14,
				bottom: 18,
				toJSON: () => ({}),
				...rect
			}) as DOMRect,
		configurable: true
	});
};

// show() is async (it awaits tick() before computing placement), so flush a macrotask to let the
// handler's continuation land.
const flush = () => new Promise((r) => setTimeout(r, 0));

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

describe('HintMarker — fixed-position placement (QURATOR-383)', () => {
	it('near the viewport top, the tip is position:fixed and flips BELOW the trigger (top >= rect.bottom)', async () => {
		const { getByRole } = render(HintMarker, { props: { text: 'hint', label: 'X' } });
		const trigger = getByRole('button') as HTMLElement;
		stubRect(trigger, { top: 10, bottom: 28, left: 40 });
		await fireEvent.mouseEnter(trigger);
		await flush();

		const tip = tipOf(trigger);
		// fixed takes the tip out of every overflow ancestor that was clipping it under the header.
		expect(tip.style.position).toBe('fixed');
		expect(tip.style.bottom).toBe('auto');
		// Flipped below: its top edge starts at or under the trigger's bottom edge (rect.bottom + gap).
		expect(parseFloat(tip.style.top)).toBeGreaterThanOrEqual(28);
	});

	it('with room above, the tip sits ABOVE the trigger and stays clamped inside the viewport', async () => {
		const { getByRole } = render(HintMarker, { props: { text: 'hint', label: 'X' } });
		const trigger = getByRole('button') as HTMLElement;
		stubRect(trigger, { top: 400, bottom: 418, left: 40 });
		await fireEvent.mouseEnter(trigger);
		await flush();

		const tip = tipOf(trigger);
		expect(tip.style.position).toBe('fixed');
		// Above: the tip's top edge is strictly above the trigger's top edge.
		expect(parseFloat(tip.style.top)).toBeLessThan(400);
		// Left clamp keeps the >=8px viewport margin regardless of the trigger's left.
		expect(parseFloat(tip.style.left)).toBeGreaterThanOrEqual(8);
		expect(parseFloat(tip.style.left)).toBeLessThanOrEqual(window.innerWidth - 8);
	});

	it('a scroll while the tip is open hides it (a fixed tip must never float detached)', async () => {
		const { getByRole } = render(HintMarker, { props: { text: 'hint', label: 'X' } });
		const trigger = getByRole('button') as HTMLElement;
		stubRect(trigger, { top: 400, bottom: 418, left: 40 });
		await fireEvent.mouseEnter(trigger);
		await flush();
		expect(tipOf(trigger).hasAttribute('hidden')).toBe(false);

		// capture:true so a scroll on ANY inner scroll container (.col, .modal-card) is caught.
		vi.spyOn(window, 'removeEventListener'); // spy only to prove teardown wiring below if needed
		await fireEvent(window, new Event('scroll'));
		expect(tipOf(trigger).hasAttribute('hidden')).toBe(true);
		expect(window.removeEventListener).toHaveBeenCalledWith('scroll', expect.any(Function), {
			capture: true
		});
	});
});
