// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import OverflowMenu from './OverflowMenu.svelte';

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
