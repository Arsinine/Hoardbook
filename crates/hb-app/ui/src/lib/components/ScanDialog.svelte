<script lang="ts">
	import { open as openDialog } from '@tauri-apps/plugin-dialog';
	import { scanDirectory, listSubdirs } from '../api.js';
	import { toast } from '../stores.js';
	import { icons } from '$lib/icons.js';
	import { serializeInclude, selectAllTopLevel } from '../scan-tree.js';
	import ScanTreeNode from './ScanTreeNode.svelte';
	import Modal from './Modal.svelte';
	import type { Collection, SubdirEntry } from '../types.js';

	interface Props {
		open?: boolean;
		initialPath?: string;
		initialAlias?: string;
		title?: string;
		onscanned?: (collection: Collection) => void;
		onclose?: () => void;
	}

	let {
		open = $bindable(false),
		initialPath = '',
		initialAlias = '',
		title = 'Add collection',
		onscanned,
		onclose
	}: Props = $props();

	let path = $state('');
	let pathAlias = $state('');
	let excludeRaw = $state('');
	let scanning = $state(false);

	// Folder-tree picker state (M8 — replaces the scan-depth slider, HANDOVER §A2.1).
	let topLevel: SubdirEntry[] | null = $state(null);
	let checked = $state(new Set<string>());
	let treeLoading = $state(false);
	let treeError = $state('');
	let loadedPath = '';
	// Not reactive on purpose — a plain transition-edge flag, never read by the template, so it
	// mustn't be part of the effect's own dependency tracking (avoids a self-triggering effect).
	let wasOpen = false;




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
		// devtest #10: "Select all" checks the top-level folders only — root-level files are always
		// included anyway, so adding them to the set would be redundant noise.
		if (topLevel) checked = selectAllTopLevel(topLevel.filter((n) => !n.is_file).map((n) => n.name));
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
			// Only report the result if the dialog is still open (user didn't cancel mid-scan).
			if (open) {
				onscanned?.(collection);
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
		onclose?.();
	}
	$effect(() => {
		if (open) {
			path = initialPath;
			pathAlias = initialAlias;
		}
	});
	// Load the tree once per open transition when a path is already known (re-scan reopen).
	$effect(() => {
		if (open && !wasOpen) {
			wasOpen = true;
			if (initialPath) loadTopLevel(initialPath);
		} else if (!open && wasOpen) {
			wasOpen = false;
		}
	});
	let selectedCount = $derived(checked.size);
</script>

<Modal open={open} width="440px" padding="0" onclose={close}>
	<div class="scan-frame">
			<!-- Header -->
			<div class="modal-header">
				<div class="modal-title">{title}</div>
				<button class="close-btn" onclick={close}>{@html icons.close}</button>
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
						<button class="btn-default btn-sm" onclick={browse}>Browse…</button>
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
						<span class="field-label">Files &amp; folders to include</span>
						{#if topLevel && topLevel.length > 0}
							<div class="tree-actions">
								<button class="link-btn" type="button" onclick={selectAll}>Select all</button>
								<button class="link-btn" type="button" onclick={clearAll}>Clear</button>
							</div>
						{/if}
					</div>
					<div class="tree-box">
						{#if treeLoading}
							<div class="tree-hint">Listing folders…</div>
						{:else if treeError}
							<div class="tree-hint tree-error">{treeError}</div>
						{:else if !path}
							<div class="tree-hint">Choose a directory above, then pick which files and folders to include.</div>
						{:else if topLevel === null}
							<div class="tree-hint">
								<button class="link-btn" type="button" onclick={() => loadTopLevel(path)}>
									List folders in this directory
								</button>
							</div>
						{:else if topLevel.length === 0}
							<div class="tree-hint">This directory is empty.</div>
						{:else}
							{#each topLevel as node (node.path)}
								<ScanTreeNode {node} rel={node.name} {checked} onToggle={toggleCheck} />
							{/each}
						{/if}
					</div>
					<span class="field-hint">
						Checked folders are scanned in full; check individual files to include just those. Root-level files are always included.{#if selectedCount > 0}
							· {selectedCount} selected{/if}
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
				<span class="footer-hint">Large folders can take a few minutes to scan</span>
				<div class="footer-actions">
					<button class="btn-ghost btn-sm" onclick={close}>Cancel</button>
					<button
						class="btn-primary btn-sm"
						onclick={handleScan}
						disabled={!path || !pathAlias || scanning}
					>
						{scanning ? 'Scanning…' : 'Start scan'}
					</button>
				</div>
			</div>
	</div>
</Modal>

<style>
	/* M15 W2: backdrop/card come from Modal.svelte (padding=0); this frame keeps the header/body/
	   footer chrome with the rounded, clipped corners the header border relies on. */
	.scan-frame { border-radius: 10px; overflow: hidden; }

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
	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */
</style>
