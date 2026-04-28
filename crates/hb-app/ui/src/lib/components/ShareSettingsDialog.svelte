<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { getShareSettings, saveShareSettings } from '$lib/api.js';
	import { toast } from '$lib/stores.js';
	import type { ShareSettings } from '$lib/types.js';

	export let open = false;
	export let slug = '';

	const dispatch = createEventDispatcher();

	let loading = false;
	let saving = false;
	let settings: ShareSettings = {
		enabled: false,
		allowed_paths: [],
		speed_cap_kbps: undefined,
		download_limit: undefined,
		require_follow: false,
	};

	let speedCapStr = '';
	let downloadLimitStr = '';
	let newPath = '';

	$: if (open && slug) loadSettings();

	async function loadSettings() {
		loading = true;
		try {
			const s = await getShareSettings(slug);
			settings = s;
			speedCapStr = s.speed_cap_kbps != null ? String(s.speed_cap_kbps) : '';
			downloadLimitStr = s.download_limit != null ? String(s.download_limit) : '';
		} catch {
			// no saved settings yet — use defaults
		} finally {
			loading = false;
		}
	}

	async function handleSave() {
		saving = true;
		try {
			const toSave: ShareSettings = {
				...settings,
				speed_cap_kbps: speedCapStr ? parseInt(speedCapStr) || undefined : undefined,
				download_limit: downloadLimitStr ? parseInt(downloadLimitStr) || undefined : undefined,
			};
			await saveShareSettings(slug, toSave);
			settings = toSave;
			toast('Share settings saved');
			dispatch('close');
			open = false;
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			saving = false;
		}
	}

	function addPath() {
		const p = newPath.trim();
		if (p && !settings.allowed_paths.includes(p)) {
			settings.allowed_paths = [...settings.allowed_paths, p];
		}
		newPath = '';
	}

	function handleBackdrop(e: MouseEvent) {
		if (e.target === e.currentTarget) {
			open = false;
			dispatch('close');
		}
	}
</script>

{#if open}
	<div class="backdrop" on:click={handleBackdrop} role="dialog" aria-modal="true">
		<div class="modal">
			<div class="modal-head">
				<div class="modal-title">Share settings</div>
				<div class="modal-sub">Control who can download from this collection</div>
			</div>

			{#if loading}
				<div class="modal-loading">Loading…</div>
			{:else}
				<div class="modal-body">
					<!-- Enable sharing -->
					<div class="row-group">
						<div class="toggle-row">
							<div>
								<div class="row-label">Enable file sharing</div>
								<div class="row-sub">Allow peers to request downloads from this collection</div>
							</div>
							<button
								class="toggle"
								class:toggle-on={settings.enabled}
								on:click={() => (settings.enabled = !settings.enabled)}
							>
								<span class="toggle-thumb" />
							</button>
						</div>
					</div>

					{#if settings.enabled}
						<div class="row-group">
							<div class="group-label">Restrictions</div>

							<div class="field">
								<label class="field-label" for="speed-cap">Speed cap (KB/s)</label>
								<input
									id="speed-cap"
									class="hb-input"
									type="number"
									min="1"
									placeholder="Unlimited"
									bind:value={speedCapStr}
								/>
							</div>

							<div class="field">
								<label class="field-label" for="dl-limit">Max simultaneous downloads</label>
								<input
									id="dl-limit"
									class="hb-input"
									type="number"
									min="1"
									placeholder="Unlimited"
									bind:value={downloadLimitStr}
								/>
							</div>

							<div class="toggle-row">
								<div>
									<div class="row-label">Followers only</div>
									<div class="row-sub">Only people you follow can download</div>
								</div>
								<button
									class="toggle"
									class:toggle-on={settings.require_follow}
									on:click={() => (settings.require_follow = !settings.require_follow)}
								>
									<span class="toggle-thumb" />
								</button>
							</div>
						</div>

						<div class="row-group">
							<div class="group-label">Allowed paths</div>
							<div class="row-sub">
								Restricts downloads to these paths only. Leave empty to allow all files.
								Use glob patterns: <code>**/*.mkv</code>, <code>Season 1/**</code>
							</div>
							{#if settings.allowed_paths.length > 0}
								<div class="path-chips">
									{#each settings.allowed_paths as p, i}
										<span class="path-chip">
											<span class="path-chip-text">{p}</span>
											<button class="path-chip-x" on:click={() => { settings.allowed_paths = settings.allowed_paths.filter((_, idx) => idx !== i); }}>×</button>
										</span>
									{/each}
								</div>
							{/if}
							<div class="path-add-row">
								<input
									class="hb-input hb-mono"
									type="text"
									placeholder="Season 1/** or **/*.mkv"
									bind:value={newPath}
									on:keydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); addPath(); } }}
								/>
								<button class="btn-default-sm" on:click={addPath} disabled={!newPath.trim()}>Add</button>
							</div>
						</div>
					{/if}
				</div>
			{/if}

			<div class="modal-foot">
				<button
					class="btn-ghost"
					on:click={() => { open = false; dispatch('close'); }}
				>Cancel</button>
				<button class="btn-primary" on:click={handleSave} disabled={saving || loading}>
					{saving ? 'Saving…' : 'Save'}
				</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: oklch(0 0 0 / 0.6);
		backdrop-filter: blur(4px);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 100;
	}

	.modal {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 12px;
		width: 420px;
		max-width: calc(100vw - 32px);
		box-shadow: 0 24px 60px oklch(0 0 0 / 0.5);
		overflow: hidden;
	}

	.modal-head {
		padding: 20px 20px 16px;
		border-bottom: 1px solid var(--divider);
	}

	.modal-title { font-size: 15px; font-weight: 600; color: var(--fg); }
	.modal-sub { font-size: 12px; color: var(--fg-muted); margin-top: 3px; }

	.modal-loading { padding: 32px; text-align: center; color: var(--fg-dim); font-size: 13px; }

	.modal-body { padding: 16px 20px; display: flex; flex-direction: column; gap: 16px; }

	.row-group {
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 12px 14px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.group-label {
		font-size: 10.5px;
		color: var(--fg-dim);
		text-transform: uppercase;
		letter-spacing: 0.8px;
		font-weight: 600;
	}

	.toggle-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; }

	.row-label { font-size: 13px; font-weight: 500; color: var(--fg); }

	.row-sub { font-size: 11.5px; color: var(--fg-dim); margin-top: 2px; line-height: 1.5; }

	.field { display: flex; flex-direction: column; gap: 5px; }

	.field-label { font-size: 11px; color: var(--fg-muted); font-weight: 500; }

	.hb-input {
		height: 34px;
		padding: 0 11px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		outline: none;
		width: 100%;
	}
	.hb-input::placeholder { color: var(--fg-dim); }
	.hb-input:focus { border-color: var(--accent); }

	.toggle {
		width: 30px; height: 17px;
		border-radius: 99px;
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		position: relative;
		flex-shrink: 0;
		cursor: pointer;
		transition: background 0.15s, border-color 0.15s;
	}
	.toggle-on { background: var(--accent); border-color: var(--accent); }
	.toggle-thumb {
		position: absolute;
		top: 1px; left: 1px;
		width: 13px; height: 13px;
		border-radius: 50%;
		background: var(--fg-muted);
		transition: left 0.15s, background 0.15s;
	}
	.toggle-on .toggle-thumb { left: 14px; background: var(--accent-text); }

	.modal-foot {
		padding: 14px 20px;
		border-top: 1px solid var(--divider);
		display: flex;
		justify-content: flex-end;
		gap: 8px;
	}

	code {
		font-family: var(--font-mono);
		font-size: 11px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 1px 4px;
		color: var(--fg);
	}

	.path-chips { display: flex; flex-wrap: wrap; gap: 5px; }
	.path-chip {
		display: inline-flex; align-items: center; gap: 3px;
		background: var(--bg-input); border: 1px solid var(--border);
		border-radius: 5px; padding: 2px 4px 2px 8px;
		font-family: var(--font-mono); font-size: 11px; color: var(--fg);
	}
	.path-chip-text { max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.path-chip-x {
		background: none; border: none; cursor: pointer;
		color: var(--fg-dim); font-size: 14px; line-height: 1;
		padding: 0; display: flex; align-items: center;
	}
	.path-chip-x:hover { color: var(--fg); }

	.path-add-row { display: flex; gap: 6px; align-items: center; }

	.hb-mono { font-family: var(--font-mono); font-size: 12px; }

	.btn-default-sm {
		display: inline-flex; align-items: center; justify-content: center;
		padding: 5px 11px; font-family: var(--font-ui); font-size: 12px; font-weight: 500;
		color: var(--fg); background: transparent;
		border: 1px solid var(--border-strong); border-radius: 6px;
		cursor: pointer; white-space: nowrap; flex-shrink: 0; height: 28px;
	}
	.btn-default-sm:disabled { opacity: 0.4; cursor: not-allowed; }

	.btn-primary {
		display: inline-flex; align-items: center; gap: 6px;
		padding: 7px 16px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; line-height: 1;
	}
	.btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

	.btn-ghost {
		display: inline-flex; align-items: center; gap: 6px;
		padding: 7px 16px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg-muted); background: transparent;
		border: 1px solid transparent; border-radius: 7px;
		cursor: pointer; line-height: 1;
	}
</style>
