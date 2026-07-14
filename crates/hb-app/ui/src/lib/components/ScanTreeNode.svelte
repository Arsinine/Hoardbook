<script lang="ts">
	import ScanTreeNode from './ScanTreeNode.svelte';
	import { listSubdirs } from '../api.js';
	import { triState } from '../scan-tree.js';
	import type { SubdirEntry } from '../types.js';
	import { icons } from '$lib/icons.js';

	interface Props {
		node: SubdirEntry;
		/** This node's relative path within the scan root ("/"-separated). */
		rel: string;
		/** Shared checked-set; the dialog reassigns it on every toggle so tri-state stays reactive. */
		checked: Set<string>;
		/** Toggle the explicit-checked membership of `rel` (the dialog owns the set). */
		onToggle: (rel: string) => void;
		depth?: number;
	}

	let { node, rel, checked, onToggle, depth = 0 }: Props = $props();

	let open = $state(false);
	let children: SubdirEntry[] | null = $state(null);
	let loading = $state(false);
	let loadError = $state('');
	let inputEl: HTMLInputElement | undefined = $state();

	let triStateValue = $derived(triState(rel, checked));
	// devtest #10: a root-level (depth 0) file is always included by the scan (loose root files), so
	// show it locked+checked — honest, and consistent with a file pulled in by a checked parent folder.
	let rootFileLocked = $derived(node.is_file === true && depth === 0);
	let locked = $derived(rootFileLocked || triStateValue === 'locked');
	let isChecked = $derived(rootFileLocked || triStateValue === 'checked' || triStateValue === 'locked');
	let lockHint = $derived(
		rootFileLocked
			? 'Root-level files are always included'
			: 'Included via a checked parent — uncheck the parent to refine'
	);
	// `indeterminate` is a DOM property, not an attribute — set it imperatively so it stays in sync.
	$effect(() => {
		if (inputEl) inputEl.indeterminate = triStateValue === 'indeterminate';
	});

	async function toggleExpand() {
		open = !open;
		if (open && children === null && node.has_children) {
			loading = true;
			loadError = '';
			try {
				children = await listSubdirs(node.path);
			} catch (e) {
				loadError = String(e);
				children = [];
			} finally {
				loading = false;
			}
		}
	}

	function onCheckChange() {
		// A locked node is included via a checked ancestor — refine by unchecking the parent.
		if (locked) return;
		onToggle(rel);
	}
</script>

<div class="tree-node">
	<div class="node-row" style="padding-left:{depth * 15}px">
		{#if node.has_children}
			<button
				class="expander"
				class:open
				type="button"
				onclick={toggleExpand}
				aria-label={open ? 'Collapse folder' : 'Expand folder'}
			>
				{@html icons.chevronRight}
			</button>
		{:else}
			<span class="expander-spacer"></span>
		{/if}
		<label class="node-label">
			<input
				bind:this={inputEl}
				type="checkbox"
				checked={isChecked}
				disabled={locked}
				onchange={onCheckChange}
			/>
			<span class="node-icon" class:file-icon={node.is_file}>{@html node.is_file ? icons.file : icons.folder}</span>
			<span class="node-name">{node.name}</span>
			{#if locked}
				<span class="lock-badge" title={lockHint}>🔒</span>
			{/if}
		</label>
	</div>

	{#if open}
		{#if loading}
			<div class="node-hint" style="padding-left:{(depth + 1) * 15 + 18}px">Loading…</div>
		{:else if loadError}
			<div class="node-hint node-error" style="padding-left:{(depth + 1) * 15 + 18}px">{loadError}</div>
		{:else if children}
			{#each children as child (child.path)}
				<ScanTreeNode
					node={child}
					rel={`${rel}/${child.name}`}
					{checked}
					{onToggle}
					depth={depth + 1}
				/>
			{/each}
		{/if}
	{/if}
</div>

<style>
	.tree-node { display: flex; flex-direction: column; }

	.node-row { display: flex; align-items: center; gap: 2px; min-height: 24px; }

	.expander {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 18px;
		height: 18px;
		flex-shrink: 0;
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-dim);
		padding: 0;
		transition: transform 0.12s ease;
	}

	.expander.open { transform: rotate(90deg); }

	@media (prefers-reduced-motion: reduce) { .expander { transition: none; } }

	.expander-spacer { width: 18px; height: 18px; flex-shrink: 0; }

	.node-label {
		display: flex;
		align-items: center;
		gap: 6px;
		cursor: pointer;
		font-size: 12.5px;
		color: var(--fg);
		min-width: 0;
	}

	.node-label input { cursor: pointer; flex-shrink: 0; }
	.node-label input:disabled { cursor: default; }

	.node-icon { color: var(--accent); display: flex; flex-shrink: 0; }
	.node-icon.file-icon { color: var(--fg-dim); }

	.node-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

	.lock-badge { font-size: 10px; opacity: 0.7; flex-shrink: 0; }

	.node-hint { font-size: 11.5px; color: var(--fg-dim); padding-top: 2px; padding-bottom: 2px; }

	.node-error { color: var(--danger, #e5484d); }
</style>
