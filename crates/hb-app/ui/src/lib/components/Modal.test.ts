// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import Modal from './Modal.svelte';

afterEach(cleanup);

// A minimal children snippet with one focusable control inside the dialog.
const body = createRawSnippet(() => ({
	render: () => `<button data-testid="inside">Inside</button>`,
}));

// QURATOR-96 repro shape: a form with two focusables, so one can be removed (focus falls to
// <body>) while the other stays reachable — Tab must land back on the survivor.
const form = createRawSnippet(() => ({
	render: () => `<div><input data-testid="f1" /><button data-testid="f2">go</button></div>`,
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

// QURATOR-96 — Escape and the Tab trap were `onkeydown` on the backdrop, relying on bubbling from
// a focused descendant. When a clicked element is re-rendered away, focus falls to <body> and the
// event never bubbles through the backdrop: Esc is dead and Tab cycles the page BEHIND the modal.
// These are behavioural mounts (events + focus work in jsdom), not source-scans: the question is
// whether a window-level keydown reaches the modal when focus is on <body>.
describe('Modal — QURATOR-96: keys survive focus escaping to <body>', () => {
	it('Escape dispatched at window fires onclose when focus is on <body>', async () => {
		const onclose = vi.fn();
		render(Modal, { props: { open: true, onclose, children: body } });
		document.body.focus(); // jsdom: blur from the dialog control back to <body>
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(onclose).toHaveBeenCalledTimes(1);
	});

	it('Tab dispatched at window stays trapped when focus is on <body>', async () => {
		render(Modal, { props: { open: true, onclose: () => {}, children: form } });
		// MUST let the mount effect's initial focus land BEFORE removing f1 — its tick() is a
		// pending promise, and if it runs after the removal it focuses f2 on its own, which made
		// an earlier version of this test pass with the trap deliberately disabled (the probe
		// that caught it reverted the window listener alone; the assertion had been measuring the
		// initial focus, not the trap).
		await waitFor(() => expect(document.activeElement).toBe(document.querySelector('[data-testid="f1"]')));
		const doomed = document.querySelector('[data-testid="f1"]') as HTMLElement;
		doomed.focus();
		doomed.remove(); // focus falls to <body> — the modal never sees a bubbling keydown again
		expect(document.activeElement).toBe(document.body); // the precondition, not the assertion

		await fireEvent.keyDown(window, { key: 'Tab' });
		// The trap must pull focus back INTO the dialog (first focusable), not let the browser
		// move it through the page behind the modal.
		expect(document.activeElement).toBe(document.querySelector('[data-testid="f2"]'));
	});

	it('stacked: body-focused Escape closes ONLY the topmost modal', async () => {
		const baseClose = vi.fn();
		const topClose = vi.fn();
		render(Modal, { props: { open: true, onclose: baseClose, children: body } });
		render(Modal, { props: { open: true, onclose: topClose, level: 'stacked', children: body } });
		document.body.focus();
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(topClose).toHaveBeenCalledTimes(1);
		expect(baseClose).not.toHaveBeenCalled();
	});

	it('a closed modal does not intercept window keys (stack hygiene)', async () => {
		const onclose = vi.fn();
		const { rerender } = render(Modal, { props: { open: true, onclose, children: body } });
		await rerender({ open: false });
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(onclose).not.toHaveBeenCalled();
	});

	it('the bubbling path still works (regression guard for the original design)', async () => {
		const onclose = vi.fn();
		const { container } = render(Modal, { props: { open: true, onclose, children: body } });
		await fireEvent.keyDown(container.querySelector('[data-testid="inside"]') as HTMLElement, { key: 'Escape' });
		expect(onclose).toHaveBeenCalledTimes(1);
	});
});
