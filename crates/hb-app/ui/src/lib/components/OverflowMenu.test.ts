// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { createRawSnippet, tick } from 'svelte';
import OverflowMenu from './OverflowMenu.svelte';
import { menuPosition } from '../menu-position.js';

afterEach(cleanup);

const items = createRawSnippet(() => ({
	render: () => `<button role="menuitem" data-testid="item">Rescan</button>`,
}));

describe('OverflowMenu — W3 shell', () => {
	it('renders nothing when closed', () => {
		const { container } = render(OverflowMenu, { props: { open: false, onclose: () => {}, children: items } });
		expect(container.querySelector('.overflow-menu')).toBeNull();
	});

	it('renders the menu + backdrop with the content snippet when open', () => {
		const { container, getByTestId } = render(OverflowMenu, { props: { open: true, onclose: () => {}, children: items } });
		expect(container.querySelector('.overflow-menu')).not.toBeNull();
		expect(container.querySelector('.menu-backdrop')).not.toBeNull();
		expect(getByTestId('item')).toBeTruthy();
	});

	it('backdrop click closes', async () => {
		const onclose = vi.fn();
		const { container } = render(OverflowMenu, { props: { open: true, onclose, children: items } });
		await fireEvent.click(container.querySelector('.menu-backdrop') as HTMLElement);
		expect(onclose).toHaveBeenCalledTimes(1);
	});

	it('Escape closes', async () => {
		const onclose = vi.fn();
		render(OverflowMenu, { props: { open: true, onclose, children: items } });
		await fireEvent.keyDown(document, { key: 'Escape' });
		expect(onclose).toHaveBeenCalledTimes(1);
	});
});

// QURATOR-209 — menus clipped at the window edge: clamp both axes, flip when there is no room
// below. The arithmetic lives in lib/menu-position.ts so it can be pinned with synthetic rects.
//
// ⚠ P-13 — jsdom computes no layout (`getBoundingClientRect()` returns zeros), so these tests
// prove the ARITHMETIC and the wiring, not that a menu is visually on-screen. Visual placement
// needs the Playwright-screenshot recipe.
describe('QURATOR-209 — menuPosition flip-and-clamp arithmetic', () => {
	const VP = { width: 1200, height: 800 };

	it('right-aligns to the trigger at the MEASURED width when there is room (260px menu: left 40, not the old 200px-guess 100)', () => {
		const p = menuPosition({ top: 370, right: 300, bottom: 400 }, { width: 260, height: 120 }, VP);
		expect(p.left).toBe(40); // 300 - 260
		expect(p.top).toBe(404); // below, no flip
	});

	it('clamps inside the RIGHT window edge when the trigger sits near it', () => {
		const p = menuPosition({ top: 370, right: 1196, bottom: 400 }, { width: 190, height: 120 }, VP);
		expect(p.left).toBe(1002); // 1200 - 190 - 8
		expect(p.left + 190).toBeLessThanOrEqual(VP.width - 8);
	});

	it('still clamps the LEFT window edge (pre-existing behaviour preserved)', () => {
		expect(menuPosition({ top: 370, right: 100, bottom: 400 }, { width: 190, height: 120 }, VP).left).toBe(8);
	});

	it('flips ABOVE the trigger when the menu would overflow the window floor (the owner-hit case)', () => {
		const p = menuPosition({ top: 760, right: 500, bottom: 790 }, { width: 190, height: 120 }, VP);
		expect(p.top).toBe(636); // 760 - 4 - 120
		expect(p.top + 120).toBeLessThanOrEqual(760 - 4); // fully visible above the trigger
	});

	it('stays BELOW when there is room — the common case keeps the below placement', () => {
		expect(menuPosition({ top: 370, right: 500, bottom: 400 }, { width: 190, height: 120 }, VP).top).toBe(404);
	});
});

// QURATOR-209 — wiring: the component must feed MEASURED rects (anchor + the rendered menu) into
// menuPosition only after the menu has rendered. Both rects are stubbed (jsdom computes no
// layout): this proves the tick() sequencing — a same-tick measurement of menuEl returns zeros
// and leaves the style at top:0px/left:0px — not real visual coverage.
describe('QURATOR-209 — OverflowMenu placement wiring', () => {
	it('applies flipped-and-clamped placement to the rendered menu once it exists', async () => {
		const anchor = document.createElement('button');
		const anchorRect = { top: 760, right: 1196, bottom: 790, left: 1166, width: 30, height: 30 };
		const menuRect = { top: 0, left: 0, right: 240, bottom: 120, width: 240, height: 120 };
		Object.defineProperty(window, 'innerWidth', { value: 1200, configurable: true });
		Object.defineProperty(window, 'innerHeight', { value: 800, configurable: true });
		const orig = HTMLElement.prototype.getBoundingClientRect;
		HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
			return (this === anchor ? anchorRect : menuRect) as unknown as DOMRect;
		};
		try {
			const { container } = render(OverflowMenu, {
				props: { open: true, anchor, onclose: () => {}, children: items },
			});
			await tick();
			await new Promise((r) => setTimeout(r, 0)); // let the effect's post-tick continuation run
			const el = container.querySelector('.overflow-menu') as HTMLElement;
			expect(el.style.top).toBe('636px'); // flipped above: 760 - 4 - 120
			expect(el.style.left).toBe('952px'); // right-clamped: 1200 - 240 - 8
		} finally {
			HTMLElement.prototype.getBoundingClientRect = orig;
		}
	});
});
