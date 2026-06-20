<script lang="ts">
	import type { DirectoryItem } from '../types.js';
	import DirItem from './DirItem.svelte';
	import { icons } from '$lib/icons.js';

	export let item: DirectoryItem;
	export let depth: number = 0;
	export let pathPrefix: string = '';

	let open = false;
	$: isFolder = item.item_type === 'Folder';
	$: fullPath = pathPrefix ? `${pathPrefix}/${item.name}` : item.name;
</script>

<li class="dir-row" style="padding-left:{depth * 18}px">
	{#if isFolder}
		<button class="dir-folder" on:click={() => (open = !open)}>
			<span class="dir-folder-icon">{@html icons.folder}</span>
			<span class="dir-folder-name">{item.name}</span>
			{#if item.children.length > 0}
				<span class="dir-count">({item.children.length})</span>
			{/if}
		</button>
		{#if open && item.children.length > 0}
			<ul class="dir-children">
				{#each item.children as child}
					<DirItem item={child} depth={depth + 1} pathPrefix={fullPath} />
				{/each}
			</ul>
		{/if}
	{:else}
		<div class="dir-file">
			<span class="dir-file-icon">{@html icons.file}</span>
			<span class="dir-file-name">{item.name}</span>
			{#if item.size}
				<span class="dir-size">{item.size}</span>
			{/if}
		</div>
	{/if}
</li>

<style>
	.dir-row { display: flex; flex-direction: column; padding: 2px 0; list-style: none; }

	.dir-folder {
		display: flex;
		align-items: center;
		gap: 6px;
		background: transparent;
		border: none;
		color: inherit;
		font-family: inherit;
		font-size: inherit;
		cursor: pointer;
		padding: 2px 0;
		width: 100%;
		text-align: left;
	}

	.dir-folder-icon { color: var(--accent); opacity: 0.9; display: flex; }

	.dir-folder-name { color: var(--fg); font-weight: 500; }

	.dir-count { color: var(--fg-dim); margin-left: auto; }

	.dir-file {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 2px 0;
	}

	.dir-file-icon { color: var(--fg-dim); display: flex; }

	.dir-file-name { color: var(--fg-muted); flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

	.dir-size { color: var(--fg-dim); font-feature-settings: 'tnum'; flex-shrink: 0; }

	.dir-children { list-style: none; padding: 0; margin: 0; }
</style>
