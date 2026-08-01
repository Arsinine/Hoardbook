// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import TopicJoinConsent from './TopicJoinConsent.svelte';

afterEach(cleanup);

describe('TopicJoinConsent (M11 — F12 consent gate)', () => {
	it('private join: renders the durable-record consent copy and gates Join behind an explicit ack', async () => {
		const joins: number[] = [];
		const { getByRole, getByLabelText } = render(TopicJoinConsent, {
			props: { isPrivate: true, onjoin: () => joins.push(1) },
		});
		// The consent copy is present and names the durable members-only record.
		const note = getByRole('note');
		expect(note.textContent?.toLowerCase()).toContain('durable');
		expect(note.textContent?.toLowerCase()).toContain('membership record');

		// The Join button is DISABLED until the user explicitly acknowledges (F12 — gate, not just prose).
		const join = getByRole('button', { name: /join topic/i }) as HTMLButtonElement;
		expect(join.disabled).toBe(true);

		await fireEvent.click(join); // a click while disabled must NOT fire join
		expect(joins.length).toBe(0);

		// Acknowledge → the button enables → clicking fires join exactly once.
		const ack = getByLabelText(/i understand and want to join/i) as HTMLInputElement;
		await fireEvent.click(ack);
		expect(join.disabled).toBe(false);
		await fireEvent.click(join);
		expect(joins.length).toBe(1);
	});

	it('public join: renders the "anyone who joins sees you" consent copy', () => {
		const { getByRole } = render(TopicJoinConsent, { props: { isPrivate: false } });
		const note = getByRole('note');
		expect(note.textContent?.toLowerCase()).toContain('anyone who joins');
	});

	it('always surfaces the INV-2 "no listing unlock" note', () => {
		const { getByText } = render(TopicJoinConsent, { props: { isPrivate: false } });
		expect(getByText(/does not unlock/i)).toBeTruthy();
	});

	it('W8 private redeem: renders the invite issuer line when issuerNpub is provided', () => {
		const npub = 'npub1abcdefghijklmnopqrstuvwx0123456789yz';
		const { getByText } = render(TopicJoinConsent, {
			props: { isPrivate: true, issuerNpub: npub },
		});
		// The issuer line is present and names the (truncated) issuer npub.
		const issuerLine = getByText(/invite from/i);
		expect(issuerLine.textContent?.toLowerCase()).toContain('invite from');
		// shortNpub truncates to first 8 + … + last 4.
		expect(issuerLine.textContent?.replace(/\s/g, '')).toContain('npub1ab');
	});

	it('W8 private redeem: hides the issuer line when issuerNpub is absent (public join)', () => {
		const { queryByText } = render(TopicJoinConsent, { props: { isPrivate: false } });
		expect(queryByText(/invite from/i)).toBeNull();
	});

	it('W8 private redeem: the commit does not fire until the ack is checked and Join clicked', async () => {
		// The redeem button opens consent; the commit (onjoin) must NOT fire on a click while the ack
		// is unchecked, then fire exactly once after ack + click (F12 gate holds for redeem too).
		const commits: number[] = [];
		const { getByRole, getByLabelText } = render(TopicJoinConsent, {
			props: {
				isPrivate: true,
				issuerNpub: 'npub1abcdefghijklmnopqrstuvwx0123456789yz',
				onjoin: () => commits.push(1),
			},
		});
		const join = getByRole('button', { name: /join topic/i }) as HTMLButtonElement;
		expect(join.disabled).toBe(true);

		// A click before acknowledging must NOT commit (the redeem must not commit without consent).
		await fireEvent.click(join);
		expect(commits.length).toBe(0);

		// Acknowledge → the button enables → clicking fires the commit exactly once.
		const ack = getByLabelText(/i understand and want to join/i) as HTMLInputElement;
		await fireEvent.click(ack);
		expect(join.disabled).toBe(false);
		await fireEvent.click(join);
		expect(commits.length).toBe(1);
	});
});
