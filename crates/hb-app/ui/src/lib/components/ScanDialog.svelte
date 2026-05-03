<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { open as openDialog } from '@tauri-apps/plugin-dialog';
	import { scanDirectory } from '../api.js';
	import { toast } from '../stores.js';
	import { icons } from '$lib/icons.js';
	import type { Collection } from '../types.js';

	const dispatch = createEventDispatcher<{ scanned: Collection; close: void }>();

	export let open = false;
	export let initialPath = '';
	export let initialAlias = '';
	export let title = 'Add collection';

	let path = '';
	let pathAlias = '';
	let depth = 3;
	let excludeRaw = '';
	let scanning = false;

	$: if (open) {
		path = initialPath;
		pathAlias = initialAlias;
	}

	async function browse() {
		const selected = await openDialog({ directory: true, multiple: false, title: 'Select directory' });
		if (selected) path = selected as string;
	}

	async function handleScan() {
		if (!path || !pathAlias) return;
		scanning = true;
		try {
			const exclude = excludeRaw.split(',').map((s) => s.trim()).filter(Boolean);
			const collection = await scanDirectory({ path, path_alias: pathAlias, depth, exclude });
			// Only dispatch result if the dialog is still open (user didn't cancel mid-scan).
			if (open) {
				dispatch('scanned', collection);
				close();
			}
		} catch (e) {
			if (open) toast(String(e), 'error');
		} finally {
			scanning = false;
		}
	}

	function close() {
		open = false;
		scanning = false; // Reset so reopening the dialog is not stuck in "Scanning…"
		path = '';
		pathAlias = '';
		depth = 3;
		dispatch('close');
	}

	$: pct = ((depth / 6) * 100).toFixed(1);
</script>

{#if open}
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<div
		class="backdrop"
		on:click|self={close}
		on:keydown={(e) => e.key === 'Escape' && close()}
		role="presentation"
	>
		<div class="modal">
			<!-- Header -->
			<div class="modal-header">
				<div class="modal-title">{title}</div>
				<button class="close-btn" on:click={close}>{@html icons.close}</button>
			</div>

			<!-- Body -->
			<div class="modal-body">
				<!-- Directory path -->
				<div class="field">
					<div class="field-label">Directory path <span class="accent-dot">•</span></div>
					<div class="path-row">
						<div class="hb-input-wrap">
							<span class="input-lead">{@html icons.folder}</span>
							<input
								class="hb-input-bare hb-mono"
								type="text"
								placeholder="C:\Movies or /mnt/data"
								bind:value={path}
							/>
						</div>
						<button class="btn-default btn-sm" on:click={browse}>Browse…</button>
					</div>
				</div>

				<!-- Display name -->
				<div class="field">
					<label class="field-label">Display name</label>
					<input class="hb-input" type="text" placeholder="Criterion Collection" bind:value={pathAlias} />
				</div>

				<!-- Scan depth -->
				<div class="field">
					<div class="slider-header">
						<span class="field-label">Scan depth</span>
						<span class="slider-val">{depth} levels</span>
					</div>
					<div class="slider-wrap">
						<input type="range" class="slider" min="1" max="6" bind:value={depth} />
					</div>
				</div>

				<!-- Exclude patterns -->
				<div class="field">
					<div class="field-label-row">
						<label class="field-label">Exclude patterns</label>
						<span class="field-hint">comma-separated, leave blank to include everything</span>
					</div>
					<input
						class="hb-input hb-mono"
						type="text"
						placeholder=".git, node_modules, __pycache__, .DS_Store, *.tmp"
						bind:value={excludeRaw}
					/>
				</div>

			</div>

			<!-- Footer -->
			<div class="modal-footer">
				<span class="footer-hint">Initial scan: ~2 minutes</span>
				<div class="footer-actions">
					<button class="btn-ghost btn-sm" on:click={close}>Cancel</button>
					<button
						class="btn-primary btn-sm"
						on:click={handleScan}
						disabled={!path || !pathAlias || scanning}
					>
						{scanning ? 'Scanning…' : 'Start scan'}
					</button>
				</div>
			</div>
		</div>
	</div>
{/if}

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: oklch(0.10 0.005 260 / 0.7);
		backdrop-filter: blur(4px);
		z-index: 100;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 30px;
	}

	.modal {
		width: 440px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 10px;
		box-shadow: 0 30px 80px -20px oklch(0 0 0 / 0.7), 0 0 0 1px oklch(1 0 0 / 0.06);
		overflow: hidden;
	}

	.modal-header {
		padding: 16px 20px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.modal-title { font-size: 15px; font-weight: 600; color: var(--fg); }

	.close-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-muted);
		display: flex;
		padding: 2px;
	}

	.modal-body {
		padding: 20px;
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.modal-footer {
		padding: 12px 20px;
		border-top: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		background: var(--bg-elev1);
	}

	.footer-hint { font-size: 11.5px; color: var(--fg-dim); }

	.footer-actions { display: flex; gap: 8px; }

	.field { display: flex; flex-direction: column; gap: 5px; }

	.field-label {
		font-size: 11px;
		color: var(--fg-muted);
		font-weight: 500;
	}

	.field-label-row { display: flex; justify-content: space-between; align-items: baseline; }

	.field-hint { font-size: 10.5px; color: var(--fg-dim); }

	.accent-dot { color: var(--accent); margin-left: 3px; }

	.path-row { display: flex; gap: 8px; }

	.hb-input-wrap {
		flex: 1;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 0 11px;
		height: 34px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}

	.hb-input-wrap span { color: var(--fg-dim); display: flex; }

	.hb-input-bare {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		min-width: 0;
	}

	.hb-input-bare::placeholder { color: var(--fg-dim); }

	.hb-input {
		display: flex;
		align-items: center;
		padding: 0 11px;
		height: 34px;
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

	.hb-mono { font-family: var(--font-mono); }

	.slider-header { display: flex; justify-content: space-between; align-items: baseline; }

	.slider-val { font-size: 13px; font-weight: 600; color: var(--fg); font-feature-settings: 'tnum'; }

	.slider-wrap { padding: 0 8px; }

	.slider {
		-webkit-appearance: none;
		appearance: none;
		width: 100%;
		height: 4px;
		background: var(--bg-elev3);
		border-radius: 99px;
		outline: none;
		margin: 6px 0;
	}

	.slider::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: var(--fg);
		border: 2px solid var(--accent);
		box-shadow: 0 2px 6px oklch(0 0 0 / 0.4);
		cursor: pointer;
	}

	/* Buttons */
	.btn-primary {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}

	.btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

	.btn-default {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg); background: transparent;
		border: 1px solid var(--border-strong); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}

	.btn-ghost {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg-muted); background: transparent;
		border: 1px solid transparent; border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}

	.btn-sm { padding: 5px 11px; font-size: 12px; }
</style>
