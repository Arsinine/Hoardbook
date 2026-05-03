<script lang="ts">
	import { contacts } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import type { CachedPeer, Collection, DirectoryItem } from '$lib/types.js';

	type BcItem =
		| { label: string; kind: 'contact' }
		| { label: string; kind: 'collection' }
		| { label: string; kind: 'folder'; index: number };

	let search = '';
	let selectedPeer: CachedPeer | null = null;
	let selectedCollection: Collection | null = null;
	let folderStack: { name: string; items: DirectoryItem[] }[] = [];

	$: filteredContacts = $contacts
		.filter(p => {
			if (!search) return true;
			const q = search.toLowerCase();
			return (
				(p.profile?.display_name?.toLowerCase().includes(q) ?? false) ||
				p.hb_id.toLowerCase().includes(q)
			);
		})
		.sort((a, b) => {
			if (a.online !== b.online) return a.online ? -1 : 1;
			const na = a.profile?.display_name ?? a.hb_id;
			const nb = b.profile?.display_name ?? b.hb_id;
			return na.localeCompare(nb);
		});

	$: currentItems = folderStack.length > 0
		? folderStack[folderStack.length - 1].items
		: (selectedCollection?.listing ?? []);

	$: sortedItems = [...currentItems].sort((a, b) => {
		if (a.item_type !== b.item_type) return a.item_type === 'Folder' ? -1 : 1;
		return a.name.localeCompare(b.name);
	});

	let breadcrumbs: BcItem[] = [];
	$: breadcrumbs = [
		...(selectedPeer ? [{ label: peerName(selectedPeer), kind: 'contact' as const }] : []),
		...(selectedCollection ? [{ label: selectedCollection.path_alias, kind: 'collection' as const }] : []),
		...folderStack.map((f, i) => ({ label: f.name, kind: 'folder' as const, index: i })),
	];

	function peerName(peer: CachedPeer): string {
		return peer.profile?.display_name ?? peer.hb_id.slice(0, 10) + '…';
	}

	function peerInitial(peer: CachedPeer): string {
		return (peer.profile?.display_name?.[0] ?? peer.hb_id[0]).toUpperCase();
	}

	function selectPeer(peer: CachedPeer) {
		selectedPeer = peer;
		selectedCollection = null;
		folderStack = [];
	}

	function selectCollection(col: Collection) {
		selectedCollection = col;
		folderStack = [];
	}

	function enterFolder(item: DirectoryItem) {
		folderStack = [...folderStack, { name: item.name, items: item.children }];
	}

	function navigateBc(bc: BcItem) {
		if (bc.kind === 'contact') {
			selectedCollection = null;
			folderStack = [];
		} else if (bc.kind === 'collection') {
			folderStack = [];
		} else {
			folderStack = folderStack.slice(0, bc.index + 1);
		}
	}

	function fmtBytes(bytes: number): string {
		if (bytes > 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
		if (bytes > 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
		if (bytes > 1e3) return (bytes / 1e3).toFixed(0) + ' KB';
		return bytes + ' B';
	}
</script>

<div class="browse-shell">
	<!-- Left: contact list -->
	<div class="left-panel">
		<div class="panel-top">
			<span class="panel-title">People</span>
		</div>
		<div class="search-wrap">
			<span class="search-icon">{@html icons.search}</span>
			<input class="search-input" placeholder="Filter contacts…" bind:value={search} />
		</div>

		<div class="contact-list">
			{#if $contacts.length === 0}
				<div class="left-empty">No contacts yet</div>
			{:else if filteredContacts.length === 0}
				<div class="left-empty">No matches</div>
			{:else}
				{#each filteredContacts as peer (peer.hb_id)}
					{@const letter = peerInitial(peer)}
					{@const hue = avatarHue(letter)}
					<button
						class="contact-row"
						class:contact-selected={selectedPeer?.hb_id === peer.hb_id}
						on:click={() => selectPeer(peer)}
					>
						<div class="avatar-wrap">
							<Avatar {letter} size={28} {hue} />
							{#if peer.online}
								<span class="online-dot" />
							{/if}
						</div>
						<div class="contact-info">
							<span class="contact-name">{peerName(peer)}</span>
							<span class="contact-meta">
								{peer.collections.length} collection{peer.collections.length !== 1 ? 's' : ''}
							</span>
						</div>
					</button>
				{/each}
			{/if}
		</div>
	</div>

	<!-- Right: browser -->
	<div class="right-panel">
		{#if !selectedPeer}
			<div class="empty-state">
				<div class="empty-icon">{@html icons.folder}</div>
				<div class="empty-label">Select a contact to browse their collections</div>
			</div>
		{:else}
			<!-- Breadcrumb -->
			<div class="breadcrumb">
				{#each breadcrumbs as bc, i}
					{#if i > 0}
						<span class="bc-sep">{@html icons.chevronRight}</span>
					{/if}
					{#if i < breadcrumbs.length - 1}
						<button class="bc-btn" on:click={() => navigateBc(bc)}>{bc.label}</button>
					{:else}
						<span class="bc-current">{bc.label}</span>
					{/if}
				{/each}
			</div>

			<!-- Collections grid -->
			{#if !selectedCollection}
				{#if selectedPeer.collections.length === 0}
					<div class="empty-state">
						<div class="empty-icon">{@html icons.folder}</div>
						<div class="empty-label">No public collections</div>
					</div>
				{:else}
					<div class="col-grid">
						{#each selectedPeer.collections as col (col.slug)}
							<button class="col-card" on:click={() => selectCollection(col)}>
								<div class="col-card-icon">{@html icons.folder}</div>
								<div class="col-card-name">{col.path_alias}</div>
								{#if col.description}
									<div class="col-card-desc">{col.description}</div>
								{/if}
								<div class="col-card-meta">
									{col.item_count} item{col.item_count !== 1 ? 's' : ''}
									{#if col.est_size}· {col.est_size}{:else if col.total_bytes}· {fmtBytes(col.total_bytes)}{/if}
								</div>
								{#if col.content_type.length > 0 || col.sorted}
									<div class="col-tags">
										{#each col.content_type.slice(0, 3) as t}
											<span class="tag">{t}</span>
										{/each}
										{#if col.sorted}
											<span class="tag tag-sorted">sorted</span>
										{/if}
									</div>
								{/if}
							</button>
						{/each}
					</div>
				{/if}

			<!-- File tree -->
			{:else}
				<div class="file-view">
					{#if sortedItems.length === 0}
						<div class="empty-state">
							<div class="empty-icon">{@html icons.folder}</div>
							<div class="empty-label">Empty folder</div>
						</div>
					{:else}
						<div class="file-table">
							<div class="file-header">
								<span class="fh-name">Name</span>
								<span class="fh-size">Size</span>
								<span class="fh-type">Type</span>
							</div>
							{#each sortedItems as item (item.name)}
								<button
									class="file-row"
									class:file-folder={item.item_type === 'Folder'}
									class:file-leaf={item.item_type === 'File'}
									on:click={() => { if (item.item_type === 'Folder') enterFolder(item); }}
								>
									<span class="file-icon">
										{@html item.item_type === 'Folder' ? icons.folder : icons.file}
									</span>
									<span class="file-name">{item.name}</span>
									<span class="file-size">{item.size ?? ''}</span>
									<span class="file-type">{item.format ?? ''}</span>
								</button>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		{/if}
	</div>
</div>

<style>
	.browse-shell {
		display: flex;
		height: 100%;
		overflow: hidden;
	}

	/* ── Left panel ──────────────────────────────────────────────── */

	.left-panel {
		width: 216px;
		flex-shrink: 0;
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	.panel-top {
		padding: 14px 14px 10px;
		border-bottom: 1px solid var(--divider);
	}

	.panel-title {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.6px;
		text-transform: uppercase;
		color: var(--fg-dim);
	}

	.search-wrap {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 10px;
		border-bottom: 1px solid var(--divider);
		color: var(--fg-dim);
		flex-shrink: 0;
	}

	.search-icon { display: flex; flex-shrink: 0; }

	.search-input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 12px;
		color: var(--fg);
		font-family: var(--font-ui);
	}

	.search-input::placeholder { color: var(--fg-dim); }

	.contact-list {
		overflow-y: auto;
		flex: 1;
	}

	.left-empty {
		padding: 16px;
		font-size: 12px;
		color: var(--fg-dim);
		text-align: center;
	}

	.contact-row {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 8px 12px;
		background: transparent;
		border: none;
		cursor: pointer;
		width: 100%;
		text-align: left;
		transition: background 0.1s;
	}

	.contact-row:hover { background: var(--bg-elev1); }
	.contact-selected { background: var(--bg-elev2) !important; }

	.avatar-wrap {
		position: relative;
		flex-shrink: 0;
	}

	.online-dot {
		position: absolute;
		bottom: -1px;
		right: -1px;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--online);
		border: 1.5px solid var(--bg);
	}

	.contact-info {
		min-width: 0;
		flex: 1;
	}

	.contact-name {
		display: block;
		font-size: 12.5px;
		font-weight: 500;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.contact-meta {
		display: block;
		font-size: 10.5px;
		color: var(--fg-dim);
		margin-top: 1px;
	}

	/* ── Right panel ─────────────────────────────────────────────── */

	.right-panel {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

	.empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 10px;
		color: var(--fg-dim);
	}

	.empty-icon {
		opacity: 0.3;
		transform: scale(2.8);
		margin-bottom: 8px;
		display: flex;
	}

	.empty-label { font-size: 12.5px; }

	/* Breadcrumb */

	.breadcrumb {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 9px 14px;
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
		flex-wrap: wrap;
		min-height: 38px;
	}

	.bc-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		font-size: 12px;
		font-weight: 500;
		color: var(--fg-muted);
		padding: 2px 5px;
		border-radius: 4px;
		font-family: var(--font-ui);
		transition: background 0.1s, color 0.1s;
	}

	.bc-btn:hover {
		background: var(--bg-elev2);
		color: var(--fg);
	}

	.bc-sep {
		color: var(--fg-dim);
		display: flex;
		align-items: center;
		padding: 0 1px;
	}

	.bc-current {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg);
		padding: 2px 5px;
	}

	/* Collections grid */

	.col-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(158px, 1fr));
		gap: 10px;
		padding: 16px;
		overflow-y: auto;
		align-content: start;
	}

	.col-card {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 12px;
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 8px;
		cursor: pointer;
		text-align: left;
		transition: background 0.1s, border-color 0.1s;
	}

	.col-card:hover {
		background: var(--bg-elev2);
		border-color: var(--border-strong);
	}

	.col-card-icon {
		color: var(--accent);
		margin-bottom: 4px;
		display: flex;
	}

	.col-card-name {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--fg);
		word-break: break-word;
	}

	.col-card-desc {
		font-size: 11px;
		color: var(--fg-muted);
		overflow: hidden;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}

	.col-card-meta {
		font-size: 10.5px;
		color: var(--fg-dim);
		margin-top: 2px;
	}

	.col-tags {
		display: flex;
		flex-wrap: wrap;
		gap: 3px;
		margin-top: 4px;
	}

	.tag {
		font-size: 9.5px;
		padding: 1px 5px;
		border-radius: 999px;
		background: var(--bg-elev3);
		color: var(--fg-muted);
		border: 1px solid var(--border);
	}

	.tag-sorted {
		background: var(--accent-soft);
		color: var(--accent);
		border-color: color-mix(in oklch, var(--accent) 30%, transparent);
	}

	/* File view */

	.file-view {
		flex: 1;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
	}

	.file-table {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.file-header {
		display: grid;
		grid-template-columns: 1fr 80px 90px;
		padding: 6px 14px 6px 40px;
		border-bottom: 1px solid var(--border);
		position: sticky;
		top: 0;
		background: var(--bg);
		z-index: 1;
		flex-shrink: 0;
	}

	.fh-name, .fh-size, .fh-type {
		font-size: 10.5px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--fg-dim);
	}

	.fh-size, .fh-type { text-align: right; }

	.file-row {
		display: grid;
		grid-template-columns: 20px 1fr 80px 90px;
		align-items: center;
		padding: 5px 14px;
		background: transparent;
		border: none;
		width: 100%;
		text-align: left;
		gap: 0;
		transition: background 0.1s;
		column-gap: 6px;
	}

	.file-row:hover { background: var(--bg-elev1); }

	.file-folder { cursor: pointer; }
	.file-leaf { cursor: default; }

	.file-icon {
		display: flex;
		align-items: center;
		color: var(--fg-muted);
		grid-column: 1;
	}

	.file-folder .file-icon { color: var(--accent); }

	.file-name {
		font-size: 12px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		grid-column: 2;
	}

	.file-size {
		font-size: 11px;
		color: var(--fg-dim);
		text-align: right;
		font-family: var(--font-mono);
		grid-column: 3;
	}

	.file-type {
		font-size: 11px;
		color: var(--fg-dim);
		text-align: right;
		grid-column: 4;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
