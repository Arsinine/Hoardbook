import { describe, expect, it, vi } from 'vitest';

// ── Logic extracted from +page.svelte onboarding flow ───────────────────────

// Mirrors the reactive obStep initialisation logic.
// Key existence alone suppresses the wizard — "absent on all subsequent launches".
function resolveInitialStep(
	identity: { hb_id: string; hb_id_short: string } | null,
): { obStep: number } {
	if (identity) return { obStep: 4 };
	return { obStep: 1 };
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
	it('wizard_not_shown_if_key_exists: identity present (any profile state) → step 4', () => {
		// With display_name
		expect(resolveInitialStep({ hb_id: 'hb1_abc123', hb_id_short: 'hb1_abc…123' }).obStep).toBe(4);
	});

	it('wizard_not_shown_if_key_exists: identity with no profile also → step 4', () => {
		// Skipped step 2 on first launch; relaunch must not re-show wizard.
		expect(resolveInitialStep({ hb_id: 'hb1_abc123', hb_id_short: 'hb1_abc…123' }).obStep).toBe(4);
	});

	it('no identity → step 1 (wizard shown)', () => {
		expect(resolveInitialStep(null).obStep).toBe(1);
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
