// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import Modal from './Modal.svelte';

afterEach(cleanup);

// A minimal children snippet with one focusable control inside the dialog.
const body = createRawSnippet(() => ({
	render: () => `<button data-testid="inside">Inside</button>`,
}));

describe('Modal — W2 shell behavior', () => {
	it('Escape (bubbling from inside the dialog) fires onclose', async () => {
		const onclose = vi.fn();
		const { container } = render(Modal, { props: { open: true, onclose, children: body } });
		// Keydown bubbles from the focused control → backdrop handler (the stacking-safe path).
		await fireEvent.keyDown(container.querySelector('[data-testid="inside"]') as HTMLElement, { key: 'Escape' });
		expect(onclose).toHaveBeenCalledTimes(1);
	});

	it('backdrop click fires onclose; a click inside the card does not', async () => {
		const onclose = vi.fn();
		const { container } = render(Modal, { props: { open: true, onclose, children: body } });
		const backdrop = container.querySelector('.modal-backdrop') as HTMLElement;
		const card = container.querySelector('.modal-card') as HTMLElement;

		await fireEvent.click(card); // inside → no close
		expect(onclose).not.toHaveBeenCalled();

		await fireEvent.click(backdrop); // on the backdrop itself → close
		expect(onclose).toHaveBeenCalledTimes(1);
	});

	it('closeOnBackdrop=false ignores backdrop clicks', async () => {
		const onclose = vi.fn();
		const { container } = render(Modal, {
			props: { open: true, onclose, closeOnBackdrop: false, children: body },
		});
		await fireEvent.click(container.querySelector('.modal-backdrop') as HTMLElement);
		expect(onclose).not.toHaveBeenCalled();
	});

	it('stacked level computes a higher z-layer than base', () => {
		const base = render(Modal, { props: { open: true, onclose: () => {}, level: 'base', children: body } });
		const baseZ = (base.container.querySelector('.modal-backdrop') as HTMLElement).getAttribute('style') ?? '';
		cleanup();
		const stacked = render(Modal, { props: { open: true, onclose: () => {}, level: 'stacked', children: body } });
		const stackedZ = (stacked.container.querySelector('.modal-backdrop') as HTMLElement).getAttribute('style') ?? '';

		expect(baseZ).toContain('--z-modal)');
		expect(baseZ).not.toContain('stacked');
		expect(stackedZ).toContain('--z-modal-stacked');
	});

	it('renders the dialog role with aria-modal and a labelled title', () => {
		const { getByRole } = render(Modal, {
			props: { open: true, onclose: () => {}, title: 'My Dialog', children: body },
		});
		const dialog = getByRole('dialog');
		expect(dialog.getAttribute('aria-modal')).toBe('true');
		expect(dialog.getAttribute('aria-labelledby')).toBeTruthy();
	});

	it('renders nothing when open=false', () => {
		const { container } = render(Modal, { props: { open: false, onclose: () => {}, children: body } });
		expect(container.querySelector('.modal-backdrop')).toBeNull();
	});
});
