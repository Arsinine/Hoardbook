<script lang="ts">
	import type { Collection } from '../types.js';
	import DirItem from './DirItem.svelte';
	import { icons } from '$lib/icons.js';

	export let collection: Collection;
	/** Set false to suppress the built-in clickable header — used by CollectionRow (M13 W5), which
	 *  renders its own compact header and just wants the listing tree underneath. */
	export let header = true;
	/** When `header` is false, the caller controls expansion directly via this prop. */
	export let expanded = false;

	$: fmt = collection.content_types?.[0] ?? '';
</script>

<div class="panel">
	{#if header}
		<button class="panel-header" on:click={() => (expanded = !expanded)}>
			<div class="panel-left">
				<div class="folder-icon">{@html icons.folder}</div>
				<div class="panel-info">
					<div class="panel-name">{collection.path_alias}</div>
					<div class="panel-meta">
						<span class="tnum">{collection.item_count.toLocaleString()} items</span>
						{#if collection.est_size}
							<span class="dot">·</span>
							<span class="tnum">{collection.est_size}</span>
						{/if}
						{#if fmt}
							<span class="dot">·</span>
							<span class="mono-sm">{fmt}</span>
						{/if}
					</div>
				</div>
			</div>
			<div class="panel-right">
				<span class="chevron" class:chevron-open={expanded}>{@html icons.chevronDown}</span>
			</div>
		</button>
	{/if}

	{#if expanded}
		<div class="panel-body">
			{#if collection.listing.length === 0}
				<span class="empty">Empty directory</span>
			{:else}
				<ul class="dir-list">
					{#each collection.listing as item}
						<DirItem {item} depth={0} />
					{/each}
				</ul>
			{/if}
		</div>
	{/if}

	<slot />
</div>

<style>
	.panel {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		overflow: hidden;
	}

	.panel-header {
		width: 100%;
		padding: 12px 14px;
		display: flex;
		justify-content: space-between;
		align-items: center;
		cursor: pointer;
		background: transparent;
		border: none;
		color: inherit;
		font-family: inherit;
		text-align: left;
	}

	.panel-header:hover { background: var(--bg-elev2); }

	.panel-left {
		display: flex;
		align-items: center;
		gap: 12px;
		min-width: 0;
		flex: 1;
	}

	.folder-icon {
		width: 32px; height: 32px;
		border-radius: 7px;
		background: var(--bg-elev3);
		color: var(--fg-muted);
		display: flex; align-items: center; justify-content: center;
		border: 1px solid var(--border);
		flex-shrink: 0;
	}

	.panel-info { min-width: 0; }

	.panel-name { font-size: 13.5px; font-weight: 600; color: var(--fg); }

	.panel-meta {
		display: flex;
		gap: 8px;
		align-items: center;
		font-size: 11.5px;
		color: var(--fg-muted);
		margin-top: 2px;
	}

	.dot { color: var(--fg-dim); }

	.tnum { font-feature-settings: 'tnum'; }

	.mono-sm { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	.panel-right { display: flex; align-items: center; color: var(--fg-muted); }

	.chevron { display: flex; transition: transform 0.15s; }

	.chevron-open { transform: rotate(180deg); }

	.panel-body {
		border-top: 1px solid var(--divider);
		padding: 10px 14px 14px;
		background: var(--bg);
	}

	.dir-list { list-style: none; margin: 0; padding: 0; font-family: var(--font-mono); font-size: 11.5px; }

	.empty { font-size: 12px; color: var(--fg-dim); }
</style>
