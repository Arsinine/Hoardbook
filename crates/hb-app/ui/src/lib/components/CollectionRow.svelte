<script lang="ts">
	// Compact read-mostly collection row (M13 W5 Slice 1) — replaces the old always-on 6-row accordion
	// editor. The [⋯] overflow menu is rendered `position: fixed`, anchored via getBoundingClientRect(),
	// so no `overflow: hidden` ancestor can clip it (the original devtest #2 bug — it lived in a
	// scrolling `.coll-list`/`.collections-pane`).
	import { createEventDispatcher } from 'svelte';
	import type { Collection } from '../types.js';
	import { deriveRowChip, menuItems, badges, type RowMenuItem } from '../collection-row-view.js';
	import CollectionPanel from './CollectionPanel.svelte';
	import ConfirmButton from './ConfirmButton.svelte';
	import { icons } from '../icons.js';

	export let collection: Collection;

	const dispatch = createEventDispatcher<{
		rescan: Collection;
		edit: Collection;
		publish: Collection;
		unpublish: Collection;
		remove: Collection;
		export: { slug: string; format: 'text' | 'markdown' };
	}>();

	let rowExpanded = false;
	let menuOpen = false;
	let exportOpen = false;
	let menuBtnEl: HTMLButtonElement;
	let menuPos = { top: 0, left: 0 };

	$: chip = deriveRowChip(collection);
	$: items = menuItems(collection);
	$: rowBadges = badges(collection);

	function fmtBytes(b: number): string {
		const GB = 1073741824, MB = 1048576, KB = 1024;
		if (b >= GB) return (b / GB).toFixed(1) + ' GB';
		if (b >= MB) return (b / MB).toFixed(1) + ' MB';
		if (b >= KB) return (b / KB).toFixed(1) + ' KB';
		return b + ' B';
	}

	function toggleMenu() {
		if (!menuOpen && menuBtnEl) {
			const r = menuBtnEl.getBoundingClientRect();
			menuPos = { top: r.bottom + 4, left: Math.max(8, r.right - 200) };
		}
		menuOpen = !menuOpen;
		exportOpen = false;
	}

	function closeMenu() {
		menuOpen = false;
		exportOpen = false;
	}

	function handleItemClick(item: RowMenuItem) {
		if (item.key === 'export') {
			exportOpen = !exportOpen;
			return;
		}
		if (item.key === 'rescan') dispatch('rescan', collection);
		else if (item.key === 'edit') dispatch('edit', collection);
		else if (item.key === 'publish') dispatch('publish', collection);
		else if (item.key === 'unpublish') dispatch('unpublish', collection);
		closeMenu();
	}

	function handleExportClick(format: 'text' | 'markdown') {
		dispatch('export', { slug: collection.slug, format });
		closeMenu();
	}

	function handleRemoveConfirm() {
		dispatch('remove', collection);
		closeMenu();
	}
</script>

<div class="row">
	<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
	<div class="row-head" on:click={() => (rowExpanded = !rowExpanded)}>
		<span class="chevron" class:chevron-open={rowExpanded}>{@html icons.chevronDown}</span>
		<div class="row-icon">{@html icons.folder}</div>
		<div class="row-info">
			<div class="row-name">{collection.path_alias}</div>
			<div class="row-meta">
				<span class="tnum">{collection.item_count.toLocaleString()} items</span>
				<span class="dot">·</span>
				<span class="tnum">{fmtBytes(collection.total_bytes)}</span>
			</div>
		</div>
		<div class="row-badges">
			{#each collection.content_types ?? [] as ct (ct)}
				<span class="ct-badge">{ct}</span>
			{/each}
			{#each rowBadges as b (b.kind)}
				<span class="row-badge" class:row-badge-private={b.kind === 'private'}>{b.label}</span>
			{/each}
			<span class="chip-status" class:chip-published={chip === 'Published'}>{chip}</span>
		</div>
		<!-- svelte-ignore a11y-click-events-have-key-events -->
		<div class="row-menu-wrap" on:click|stopPropagation>
			<button
				type="button"
				class="row-menu-btn"
				aria-label="Collection actions"
				aria-haspopup="true"
				aria-expanded={menuOpen}
				bind:this={menuBtnEl}
				on:click={toggleMenu}
			>⋯</button>
		</div>
	</div>

	{#if rowExpanded}
		<div class="row-body">
			<CollectionPanel {collection} header={false} expanded={true} />
		</div>
	{/if}
</div>

{#if menuOpen}
	<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
	<div class="menu-backdrop" on:click={closeMenu} />
	<div class="row-menu" role="menu" style="top:{menuPos.top}px; left:{menuPos.left}px">
		{#each items as item (item.key)}
			{#if item.key === 'export'}
				<button type="button" role="menuitem" class="menu-item" on:click={() => handleItemClick(item)}>
					{item.label}<span class="submenu-caret" aria-hidden="true">▸</span>
				</button>
				{#if exportOpen}
					<div class="submenu">
						{#each item.submenu as sub (sub.key)}
							<button type="button" role="menuitem" class="menu-item menu-item-sub" on:click={() => handleExportClick(sub.key)}>
								{sub.label}
							</button>
						{/each}
					</div>
				{/if}
			{:else if item.key === 'remove'}
				<div class="menu-item menu-item-confirm">
					<ConfirmButton role="menuitem" label={item.label} on:confirm={handleRemoveConfirm} />
				</div>
			{:else}
				<button type="button" role="menuitem" class="menu-item" on:click={() => handleItemClick(item)}>
					{item.label}
				</button>
			{/if}
		{/each}
	</div>
{/if}

<style>
	.row {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		overflow: hidden;
	}

	.row-head {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 12px;
		cursor: pointer;
	}
	.row-head:hover { background: var(--bg-elev2); }

	.chevron { display: flex; color: var(--fg-muted); transition: transform 0.15s; flex-shrink: 0; }
	.chevron-open { transform: rotate(180deg); }

	.row-icon {
		width: 30px; height: 30px;
		border-radius: 7px;
		background: var(--bg-elev3);
		color: var(--fg-muted);
		display: flex; align-items: center; justify-content: center;
		border: 1px solid var(--border);
		flex-shrink: 0;
	}

	.row-info { min-width: 0; flex-shrink: 1; }
	.row-name { font-size: 13px; font-weight: 600; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.row-meta { display: flex; gap: 6px; align-items: center; font-size: 11px; color: var(--fg-muted); margin-top: 1px; }
	.dot { color: var(--fg-dim); }
	.tnum { font-feature-settings: 'tnum'; }

	.row-badges { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; margin-left: auto; }

	.ct-badge {
		font-size: 10px; padding: 1px 7px; border-radius: 999px;
		background: color-mix(in oklch, var(--accent) 10%, transparent);
		color: var(--accent);
		border: 1px solid color-mix(in oklch, var(--accent) 20%, transparent);
		text-transform: capitalize;
	}

	.row-badge {
		font-size: 10px; padding: 1px 7px; border-radius: 999px;
		background: var(--bg-elev3); color: var(--fg-muted); border: 1px solid var(--border);
	}
	.row-badge-private { color: var(--accent); border-color: color-mix(in oklch, var(--accent) 30%, transparent); }

	.chip-status {
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.6px;
		color: oklch(0.75 0.14 60);
		background: oklch(0.25 0.06 60);
		border: 1px solid oklch(0.45 0.10 60 / 0.4);
		border-radius: 4px;
		padding: 1px 6px;
		flex-shrink: 0;
	}
	.chip-published {
		color: var(--online);
		background: color-mix(in oklch, var(--online) 14%, transparent);
		border-color: color-mix(in oklch, var(--online) 25%, transparent);
	}

	.row-menu-wrap { flex-shrink: 0; }

	.row-menu-btn {
		width: 26px; height: 26px;
		display: flex; align-items: center; justify-content: center;
		background: transparent;
		border: 1px solid transparent;
		border-radius: 6px;
		color: var(--fg-muted);
		font-size: 15px;
		line-height: 1;
		cursor: pointer;
	}
	.row-menu-btn:hover { background: var(--bg-elev3); border-color: var(--border); color: var(--fg); }

	.row-body { border-top: 1px solid var(--divider); }

	/* Overflow menu — position:fixed so no ancestor's overflow:hidden clips it. */
	.menu-backdrop { position: fixed; inset: 0; z-index: 999; }

	.row-menu {
		position: fixed;
		z-index: 1000;
		min-width: 190px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 4px;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.3);
	}

	.menu-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		text-align: left;
		padding: 7px 10px;
		font-family: var(--font-ui);
		font-size: 12.5px;
		color: var(--fg);
		background: transparent;
		border: none;
		border-radius: 5px;
		cursor: pointer;
	}
	.menu-item:hover { background: var(--bg-elev3); }

	.submenu-caret { color: var(--fg-dim); font-size: 10px; }

	.submenu { padding-left: 10px; }
	.menu-item-sub { font-size: 12px; color: var(--fg-muted); }

	.menu-item-confirm { padding: 3px 6px; display: flex; }
</style>
