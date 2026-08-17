// @vitest-environment jsdom
// QURATOR-102 — the shared EmptyState component's contract: plain / icon / CTA / error variants.
// Assertions target the AFFORDANCES (role=button, role=alert, href), not words floating anywhere —
// the §9 sentinel-collision rule (asserting the string "retry" is satisfied by any prose).
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import EmptyState from './EmptyState.svelte';
import { icons } from '$lib/icons.js';

afterEach(cleanup);

describe('EmptyState — variants (QURATOR-102)', () => {
	it('plain variant renders the message with no icon, CTA, or Retry affordance', () => {
		const { container, getByText, queryByRole } = render(EmptyState, {
			props: { message: 'No collections yet.' },
		});
		expect(getByText('No collections yet.')).toBeTruthy();
		// No affordances: not an svg host, no link, no button anywhere.
		expect(container.querySelector('.hb-empty-icon svg')).toBeNull();
		expect(queryByRole('link')).toBeNull();
		expect(queryByRole('button')).toBeNull();
	});

	it('icon variant renders the given SVG from lib/icons.ts (never emoji)', () => {
		const { container } = render(EmptyState, {
			props: { message: 'No public collections', icon: icons.folder },
		});
		const svg = container.querySelector('.hb-empty-icon svg');
		expect(svg).not.toBeNull();
	});

	it('CTA variant renders an anchor with the label and href', () => {
		const { getByRole } = render(EmptyState, {
			props: { message: 'No conversations yet.', cta: { label: 'Find hoarders in Contacts →', href: '/contacts' } },
		});
		const link = getByRole('link', { name: /find hoarders in contacts/i }) as HTMLAnchorElement;
		expect(link.getAttribute('href')).toBe('/contacts');
	});

	it('error variant renders role=alert + a Retry BUTTON wired to onretry', async () => {
		const onretry = vi.fn();
		const { getByRole, container } = render(EmptyState, {
			props: { message: "Couldn't load collections", error: true, onretry },
		});
		// role=alert is the accessible error affordance — the visual distinction is .hb-empty-error.
		expect(getByRole('alert')).toBeTruthy();
		expect(container.querySelector('.hb-empty-error')).not.toBeNull();
		const retry = getByRole('button', { name: /retry/i });
		await fireEvent.click(retry);
		expect(onretry).toHaveBeenCalledTimes(1);
	});

	it('error variant without onretry still announces the error (no inert Retry button)', () => {
		const { getByRole, queryByRole } = render(EmptyState, {
			props: { message: 'load failed', error: true },
		});
		expect(getByRole('alert')).toBeTruthy();
		// A Retry button with no handler would be a dead affordance — it must not render.
		expect(queryByRole('button', { name: /retry/i })).toBeNull();
	});

	it('plain variant is NOT an alert (a genuine empty is not an error)', () => {
		const { queryByRole } = render(EmptyState, { props: { message: 'genuinely empty' } });
		expect(queryByRole('alert')).toBeNull();
	});
});
