<script lang="ts">
	// Compact read-mostly collection row (M13 W5 Slice 1) — replaces the old always-on 6-row accordion
	// editor. The [⋯] overflow menu is rendered `position: fixed`, anchored via getBoundingClientRect(),
	// so no `overflow: hidden` ancestor can clip it (the original devtest #2 bug — it lived in a
	// scrolling `.coll-list`/`.collections-pane`).
	import type { Collection } from '../types.js';
	import { deriveRowChip, menuItems, badges, type RowMenuItem } from '../collection-row-view.js';
	import CollectionPanel from './CollectionPanel.svelte';
	import ConfirmButton from './ConfirmButton.svelte';
	import OverflowMenu from './OverflowMenu.svelte';
	import { icons } from '../icons.js';

	interface Props {
		collection: Collection;
		onrescan?: (collection: Collection) => void;
		onedit?: (collection: Collection) => void;
		onpublish?: (collection: Collection) => void;
		onunpublish?: (collection: Collection) => void;
		onremove?: (collection: Collection) => void;
		onexport?: (detail: { slug: string; format: 'text' | 'markdown' }) => void;
	}

	let { collection, onrescan, onedit, onpublish, onunpublish, onremove, onexport }: Props = $props();

	let rowExpanded = $state(false);
	let menuOpen = $state(false);
	let exportOpen = $state(false);
	let menuBtnEl: HTMLButtonElement | undefined = $state();

	let chip = $derived(deriveRowChip(collection));
	let items = $derived(menuItems(collection));
	let rowBadges = $derived(badges(collection));

	function fmtBytes(b: number): string {
		const GB = 1073741824, MB = 1048576, KB = 1024;
		if (b >= GB) return (b / GB).toFixed(1) + ' GB';
		if (b >= MB) return (b / MB).toFixed(1) + ' MB';
		if (b >= KB) return (b / KB).toFixed(1) + ' KB';
		return b + ' B';
	}

	function toggleMenu() {
		menuOpen = !menuOpen; // OverflowMenu computes its own placement from the anchor
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
		if (item.key === 'rescan') onrescan?.(collection);
		else if (item.key === 'edit') onedit?.(collection);
		else if (item.key === 'publish') onpublish?.(collection);
		else if (item.key === 'unpublish') onunpublish?.(collection);
		closeMenu();
	}

	function handleExportClick(format: 'text' | 'markdown') {
		onexport?.({ slug: collection.slug, format });
		closeMenu();
	}

	function handleRemoveConfirm() {
		onremove?.(collection);
		closeMenu();
	}
</script>

<div class="row">
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="row-head" onclick={() => (rowExpanded = !rowExpanded)}>
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
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<div class="row-menu-wrap" onclick={(e) => e.stopPropagation()}>
			<button
				type="button"
				class="row-menu-btn"
				aria-label="Collection actions"
				aria-haspopup="true"
				aria-expanded={menuOpen}
				bind:this={menuBtnEl}
				onclick={toggleMenu}
			>⋯</button>
		</div>
	</div>

	{#if rowExpanded}
		<div class="row-body">
			<CollectionPanel {collection} header={false} expanded={true} />
		</div>
	{/if}
</div>

<OverflowMenu open={menuOpen} anchor={menuBtnEl} onclose={closeMenu}>
	{#each items as item (item.key)}
		{#if item.key === 'export'}
			<button type="button" role="menuitem" class="menu-item" onclick={() => handleItemClick(item)}>
				{item.label}<span class="submenu-caret" aria-hidden="true">▸</span>
			</button>
			{#if exportOpen}
				<div class="submenu">
					{#each item.submenu as sub (sub.key)}
						<button type="button" role="menuitem" class="menu-item menu-item-sub" onclick={() => handleExportClick(sub.key)}>
							{sub.label}
						</button>
					{/each}
				</div>
			{/if}
		{:else if item.key === 'remove'}
			<div class="menu-item menu-item-confirm">
				<ConfirmButton role="menuitem" label={item.label} onconfirm={handleRemoveConfirm} />
			</div>
		{:else}
			<button type="button" role="menuitem" class="menu-item" onclick={() => handleItemClick(item)}>
				{item.label}
			</button>
		{/if}
	{/each}
</OverflowMenu>

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

	/* M15 W3: backdrop + fixed-position shell moved to OverflowMenu.svelte; these style its contents. */
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
