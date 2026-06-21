<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { open as openDialog } from '@tauri-apps/plugin-dialog';
	import { scanDirectory, listSubdirs } from '../api.js';
	import { toast } from '../stores.js';
	import { icons } from '$lib/icons.js';
	import { serializeInclude, selectAllTopLevel } from '../scan-tree.js';
	import ScanTreeNode from './ScanTreeNode.svelte';
	import type { Collection, SubdirEntry } from '../types.js';

	const dispatch = createEventDispatcher<{ scanned: Collection; close: void }>();

	export let open = false;
	export let initialPath = '';
	export let initialAlias = '';
	export let title = 'Add collection';

	let path = '';
	let pathAlias = '';
	let excludeRaw = '';
	let scanning = false;

	// Folder-tree picker state (M8 — replaces the scan-depth slider, HANDOVER §A2.1).
	let topLevel: SubdirEntry[] | null = null;
	let checked = new Set<string>();
	let treeLoading = false;
	let treeError = '';
	let loadedPath = '';
	let wasOpen = false;

	$: if (open) {
		path = initialPath;
		pathAlias = initialAlias;
	}

	// Load the tree once per open transition when a path is already known (re-scan reopen).
	$: {
		if (open && !wasOpen) {
			wasOpen = true;
			if (initialPath) loadTopLevel(initialPath);
		} else if (!open && wasOpen) {
			wasOpen = false;
		}
	}

	$: selectedCount = checked.size;

	async function loadTopLevel(p: string) {
		if (!p) return;
		treeLoading = true;
		treeError = '';
		checked = new Set();
		topLevel = null;
		try {
			topLevel = await listSubdirs(p);
			loadedPath = p;
		} catch (e) {
			treeError = String(e);
			topLevel = [];
		} finally {
			treeLoading = false;
		}
	}

	async function browse() {
		const selected = await openDialog({ directory: true, multiple: false, title: 'Select directory' });
		if (selected) {
			path = selected as string;
			await loadTopLevel(path);
		}
	}

	function toggleCheck(rel: string) {
		if (checked.has(rel)) checked.delete(rel);
		else checked.add(rel);
		checked = checked; // reassign so reactivity flows through the recursive tree
	}

	function selectAll() {
		if (topLevel) checked = selectAllTopLevel(topLevel.map((n) => n.name));
	}

	function clearAll() {
		checked = new Set();
	}

	async function handleScan() {
		if (!path || !pathAlias) return;
		scanning = true;
		try {
			const exclude = excludeRaw.split(',').map((s) => s.trim()).filter(Boolean);
			const include = serializeInclude(checked);
			const collection = await scanDirectory({ path, path_alias: pathAlias, include, exclude });
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
		topLevel = null;
		checked = new Set();
		treeError = '';
		loadedPath = '';
		dispatch('close');
	}
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

				<!-- Folder selection tree -->
				<div class="field">
					<div class="field-label-row">
						<span class="field-label">Folders to include</span>
						{#if topLevel && topLevel.length > 0}
							<div class="tree-actions">
								<button class="link-btn" type="button" on:click={selectAll}>Select all</button>
								<button class="link-btn" type="button" on:click={clearAll}>Clear</button>
							</div>
						{/if}
					</div>
					<div class="tree-box">
						{#if treeLoading}
							<div class="tree-hint">Listing folders…</div>
						{:else if treeError}
							<div class="tree-hint tree-error">{treeError}</div>
						{:else if !path}
							<div class="tree-hint">Choose a directory to pick folders.</div>
						{:else if topLevel === null}
							<div class="tree-hint">
								<button class="link-btn" type="button" on:click={() => loadTopLevel(path)}>
									List folders in this directory
								</button>
							</div>
						{:else if topLevel.length === 0}
							<div class="tree-hint">No sub-folders — root-level files will be included.</div>
						{:else}
							{#each topLevel as node (node.path)}
								<ScanTreeNode {node} rel={node.name} {checked} onToggle={toggleCheck} />
							{/each}
						{/if}
					</div>
					<span class="field-hint">
						Checked folders are scanned in full; root-level files are always included.{#if selectedCount > 0}
							· {selectedCount} folder{selectedCount !== 1 ? 's' : ''} selected{/if}
					</span>
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

	/* Folder-tree picker */
	.tree-actions { display: flex; gap: 10px; }

	.link-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--accent);
		font-family: var(--font-ui);
		font-size: 11px;
		padding: 0;
	}

	.link-btn:hover { text-decoration: underline; }

	.tree-box {
		max-height: 200px;
		overflow-y: auto;
		padding: 8px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}

	.tree-hint { font-size: 12px; color: var(--fg-dim); padding: 4px 2px; }

	.tree-error { color: var(--danger, #e5484d); }

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
