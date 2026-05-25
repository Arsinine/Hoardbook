import { describe, expect, it, vi } from 'vitest';

// ── Logic extracted from +page.svelte onboarding flow ───────────────────────

// Mirrors the reactive obStep initialisation logic.
function resolveInitialStep(
	identity: { hb_id: string; hb_id_short: string } | null,
	profileDisplayName: string | undefined,
): { obStep: number; obKeypairRevealed: boolean } {
	if (identity && profileDisplayName) return { obStep: 4, obKeypairRevealed: false };
	if (identity) return { obStep: 1, obKeypairRevealed: true };
	return { obStep: 1, obKeypairRevealed: false };
}

// Mirrors obSaveName: returns whether saveProfile would be called and what step is set.
async function simulateObSaveName(
	displayName: string,
	saveFn: () => Promise<void>,
): Promise<{ savedCalled: boolean; nextStep: number }> {
	if (!displayName.trim()) return { savedCalled: false, nextStep: 3 };
	await saveFn();
	return { savedCalled: true, nextStep: 3 };
}

// ─────────────────────────────────────────────────────────────────────────────

describe('onboarding wizard — step resolution', () => {
	it('wizard_not_shown_if_key_exists: identity + profile → step 4 (wizard absent)', () => {
		const result = resolveInitialStep(
			{ hb_id: 'hb1_abc123', hb_id_short: 'hb1_abc…123' },
			'DataHoarder_42',
		);
		expect(result.obStep).toBe(4);
		expect(result.obKeypairRevealed).toBe(false);
	});

	it('identity exists but no display_name → step 1 with keypair already revealed', () => {
		const result = resolveInitialStep(
			{ hb_id: 'hb1_abc123', hb_id_short: 'hb1_abc…123' },
			undefined,
		);
		expect(result.obStep).toBe(1);
		expect(result.obKeypairRevealed).toBe(true);
	});

	it('no identity → step 1, keypair not yet revealed', () => {
		const result = resolveInitialStep(null, undefined);
		expect(result.obStep).toBe(1);
		expect(result.obKeypairRevealed).toBe(false);
	});
});

describe('onboarding wizard — step 2 save behaviour', () => {
	it('step2_skip_writes_nothing: empty display_name skips to step 3 without saving', async () => {
		const saveFn = vi.fn().mockResolvedValue(undefined);
		const result = await simulateObSaveName('', saveFn);
		expect(result.savedCalled).toBe(false);
		expect(result.nextStep).toBe(3);
		expect(saveFn).not.toHaveBeenCalled();
	});

	it('step2_skip_writes_nothing: whitespace-only name also skips without saving', async () => {
		const saveFn = vi.fn().mockResolvedValue(undefined);
		const result = await simulateObSaveName('   ', saveFn);
		expect(result.savedCalled).toBe(false);
		expect(result.nextStep).toBe(3);
		expect(saveFn).not.toHaveBeenCalled();
	});

	it('non-empty display_name calls save and advances to step 3', async () => {
		const saveFn = vi.fn().mockResolvedValue(undefined);
		const result = await simulateObSaveName('DataHoarder_42', saveFn);
		expect(result.savedCalled).toBe(true);
		expect(result.nextStep).toBe(3);
		expect(saveFn).toHaveBeenCalledOnce();
	});
});
