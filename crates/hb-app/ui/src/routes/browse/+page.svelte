<script lang="ts">
	import { contacts, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import FeatureTooltip from '$lib/components/FeatureTooltip.svelte';
	import { searchPeers, follow, getContacts, type PeerSearchHit } from '$lib/api.js';
	import { renderFingerprint } from '$lib/identity-display.js';
	import { DISCOVER_CONTENT_TYPES, parseTagInput, canSearch, toggleContentType } from '$lib/discover-view.js';
	import type { CachedPeer, Collection, DirectoryItem } from '$lib/types.js';

	type BcItem =
		| { label: string; kind: 'contact' }
		| { label: string; kind: 'collection' }
		| { label: string; kind: 'folder'; index: number };

	let search = '';
	let selectedPeer: CachedPeer | null = null;
	let selectedCollection: Collection | null = null;
	let folderStack: { name: string; items: DirectoryItem[] }[] = [];

	// ── §6 Discovery (M12 W3) — search public teasers across the relays ───────────
	let discoverTags = '';
	let discoverTypes: string[] = [];
	let discoverResults: PeerSearchHit[] = [];
	let discovering = false;
	let discoverError = '';
	let discovered = false; // a search has run at least once (drives the empty-vs-no-results copy)
	$: parsedDiscoverTags = parseTagInput(discoverTags);
	$: canDiscover = canSearch(parsedDiscoverTags, discoverTypes);
	$: followedNpubs = new Set($contacts.map((c) => c.npub));

	async function runDiscover() {
		if (!canDiscover) {
			discoverError = 'Enter at least one tag or content type to search.';
			return;
		}
		discovering = true;
		discoverError = '';
		try {
			discoverResults = await searchPeers(parsedDiscoverTags, discoverTypes);
			discovered = true;
		} catch (e) {
			discoverError = String(e);
		} finally {
			discovering = false;
		}
	}

	async function followHit(hit: PeerSearchHit) {
		try {
			await follow(hit.npub); // follow-only (bare npub): grants awareness, NOT a browse-key (INV-2)
			toast(`Following ${hit.display_name || hit.npub.slice(0, 12)}…`, 'success');
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	$: filteredContacts = $contacts
		.filter(p => {
			if (!search) return true;
			const q = search.toLowerCase();
			return (
				(p.profile?.display_name?.toLowerCase().includes(q) ?? false) ||
				p.npub.toLowerCase().includes(q)
			);
		})
		.sort((a, b) => {
			if (a.online !== b.online) return a.online ? -1 : 1;
			const na = a.profile?.display_name ?? a.npub;
			const nb = b.profile?.display_name ?? b.npub;
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

	// Feature-tooltip anchor data (HOARDBOOK_SPEC §8).
	$: peerWillingTo = selectedPeer?.profile?.willing_to ?? [];
	// A peer followed by bare npub (no share code) has sealed listings — they can't be decrypted.
	$: listingsLocked = !!selectedPeer && !selectedPeer.browse_key_hex && selectedPeer.collections.length === 0;

	function peerName(peer: CachedPeer): string {
		return peer.profile?.display_name ?? peer.npub.slice(0, 10) + '…';
	}

	function peerInitial(peer: CachedPeer): string {
		return (peer.profile?.display_name?.[0] ?? peer.npub[0]).toUpperCase();
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

	// Build the relative path for a file within the collection.
	function itemPath(item: DirectoryItem): string {
		return [...folderStack.map(f => f.name), item.name].join('/');
	}

	// ── Context menu ────────────────────────────────────────────────────────────
	let ctxMenu: { x: number; y: number; item: DirectoryItem } | null = null;

	function openCtxMenu(e: MouseEvent, item: DirectoryItem) {
		if (item.item_type !== 'File') return;
		e.preventDefault();
		ctxMenu = { x: e.clientX, y: e.clientY, item };
	}

	function closeCtxMenu() { ctxMenu = null; }

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
				{#each filteredContacts as peer (peer.npub)}
					{@const letter = peerInitial(peer)}
					{@const hue = avatarHue(letter)}
					<button
						class="contact-row"
						class:contact-selected={selectedPeer?.npub === peer.npub}
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
			<!-- §6 Discovery: find peers by tag / content-type across the relays. Results are the opt-in
			     public teaser only — each peer's listings stay 🔒 browse-key-locked (DISC3). -->
			<div class="discover">
				<div class="discover-head">
					<span class="discover-title">Discover hoarders</span>
					<span class="discover-sub">Search public profiles by tag &amp; content type — without anyone parsing your encrypted listings</span>
				</div>
				<div class="discover-filters">
					<div class="ct-row">
						{#each DISCOVER_CONTENT_TYPES as ct (ct.value)}
							<button
								type="button"
								class="ct-chip"
								class:ct-on={discoverTypes.includes(ct.value)}
								on:click={() => (discoverTypes = toggleContentType(discoverTypes, ct.value))}
							>{ct.label}</button>
						{/each}
					</div>
					<form class="tag-row" on:submit|preventDefault={runDiscover}>
						<input class="tag-input" placeholder="tags (e.g. anime, vhs)" bind:value={discoverTags} />
						<button class="search-btn" type="submit" disabled={!canDiscover || discovering}>
							{discovering ? 'Searching…' : 'Search'}
						</button>
					</form>
					{#if discoverError}<div class="discover-error">{discoverError}</div>{/if}
				</div>

				<div class="discover-results">
					{#if discovering}
						<div class="discover-empty">Searching the relays…</div>
					{:else if discovered && discoverResults.length === 0}
						<div class="discover-empty">No hoarders matched those filters.</div>
					{:else if !discovered}
						<div class="discover-empty">Pick a content type or enter a tag, then Search.</div>
					{:else}
						{#each discoverResults as hit (hit.npub)}
							{@const letter = (hit.display_name?.[0] ?? hit.npub[0]).toUpperCase()}
							{@const isContact = followedNpubs.has(hit.npub)}
							<div class="hit-card">
								<div class="hit-top">
									<Avatar {letter} size={30} hue={avatarHue(letter)} />
									<div class="hit-id">
										<span class="hit-name">{hit.display_name || hit.npub.slice(0, 12) + '…'}</span>
										{#if !isContact}
											<span class="hit-stranger" title="Verify the fingerprint before trusting a stranger">unverified — not in your contacts</span>
										{/if}
									</div>
									{#if isContact}
										<span class="hit-following">Following</span>
									{:else}
										<button class="hit-follow" on:click={() => followHit(hit)}>Follow</button>
									{/if}
								</div>
								{#if hit.bio}<div class="hit-bio">{hit.bio}</div>{/if}
								{#if hit.fingerprint}
									<div class="hit-fp" title="§7 identity fingerprint — check it before trusting a stranger">
										<span class="hit-fp-swatch" style="background:{hit.fingerprint.colorHex}"></span>
										{renderFingerprint(hit.fingerprint)}
									</div>
								{/if}
								{#if hit.content_types.length > 0 || hit.tags.length > 0}
									<div class="hit-tags">
										{#each hit.content_types as ct}<span class="tag tag-ct">{ct}</span>{/each}
										{#each hit.tags.slice(0, 6) as t}<span class="tag">#{t}</span>{/each}
									</div>
								{/if}
								<div class="hit-locked">
									🔒 Listings locked<FeatureTooltip key="listings-locked" />
								</div>
							</div>
						{/each}
					{/if}
				</div>
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

			<!-- Willing-to hints for the selected peer (off-platform exchange preferences) -->
			{#if !selectedCollection && peerWillingTo.length > 0}
				<div class="willing-bar">
					<span class="willing-label">
						Willing to<FeatureTooltip key="willing-to" />
					</span>
					{#each peerWillingTo as w}
						<span class="willing-chip">{w}</span>
					{/each}
				</div>
			{/if}

			<!-- Collections grid -->
			{#if !selectedCollection}
				{#if listingsLocked}
					<div class="empty-state">
						<div class="empty-icon">{@html icons.folder}</div>
						<div class="empty-label">
							🔒 Listings locked<FeatureTooltip key="listings-locked" />
						</div>
					</div>
				{:else if selectedPeer.collections.length === 0}
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
								{#if (col.content_types?.length ?? 0) > 0 || col.sorted}
									<div class="col-tags">
										{#each (col.content_types ?? []).slice(0, 3) as t}
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
									on:contextmenu={(e) => openCtxMenu(e, item)}
									title={item.item_type === 'File' ? 'Right-click to copy path' : undefined}
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
					<!-- The listing is metadata only — Hoardbook moves no files (H4/INV-4). -->
					<div class="no-download-note">
						<span>Metadata only — Hoardbook moves no files.</span>
						<FeatureTooltip key="no-download" />
					</div>
				</div>
			{/if}
		{/if}
	</div>
</div>

<!-- Context menu -->
{#if ctxMenu}
	<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
	<div class="ctx-backdrop" on:click={closeCtxMenu} />
	<div class="ctx-menu" style="left:{ctxMenu.x}px;top:{ctxMenu.y}px">
		<button class="ctx-item" on:click={() => {
			if (ctxMenu) navigator.clipboard.writeText(itemPath(ctxMenu.item)).catch(() => {});
			closeCtxMenu();
		}}>
			<span class="ctx-icon">{@html icons.copy}</span>
			Copy path
		</button>
	</div>
{/if}

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
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
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

	.empty-label { font-size: 12.5px; display: inline-flex; align-items: center; }

	/* Willing-to hints bar */
	.willing-bar {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 6px;
		padding: 8px 16px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}

	.willing-label {
		display: inline-flex;
		align-items: center;
		font-size: 10.5px;
		font-weight: 600;
		letter-spacing: 0.3px;
		text-transform: uppercase;
		color: var(--fg-dim);
	}

	.willing-chip {
		font-size: 11px;
		padding: 2px 8px;
		border-radius: 999px;
		background: var(--accent-soft);
		color: var(--accent);
		border: 1px solid color-mix(in oklch, var(--accent) 30%, transparent);
	}

	/* No-download footer note */
	.no-download-note {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 10px 16px;
		margin-top: auto;
		border-top: 1px solid var(--divider);
		font-size: 11.5px;
		color: var(--fg-dim);
		flex-shrink: 0;
	}

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
		grid-template-columns: 1fr 80px 90px 28px;
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
		grid-template-columns: 20px 1fr 80px 90px 28px;
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

	/* ── §6 Discovery panel ──────────────────────────────────────── */

	.discover {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

	.discover-head {
		padding: 16px 18px 10px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}

	.discover-title { font-size: 14px; font-weight: 700; color: var(--fg); }
	.discover-sub { display: block; font-size: 11.5px; color: var(--fg-dim); margin-top: 3px; }

	.discover-filters {
		padding: 12px 18px;
		border-bottom: 1px solid var(--divider);
		display: flex;
		flex-direction: column;
		gap: 10px;
		flex-shrink: 0;
	}

	.ct-row { display: flex; flex-wrap: wrap; gap: 6px; }

	.ct-chip {
		font-size: 11.5px;
		padding: 4px 11px;
		border-radius: 999px;
		background: var(--bg-elev1);
		color: var(--fg-muted);
		border: 1px solid var(--border);
		cursor: pointer;
		font-family: var(--font-ui);
		transition: background 0.1s, color 0.1s, border-color 0.1s;
	}

	.ct-chip:hover { background: var(--bg-elev2); }

	.ct-on {
		background: var(--accent-soft);
		color: var(--accent);
		border-color: color-mix(in oklch, var(--accent) 35%, transparent);
		font-weight: 600;
	}

	.tag-row { display: flex; gap: 8px; }

	.tag-input {
		flex: 1;
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 7px 10px;
		font-size: 12.5px;
		color: var(--fg);
		font-family: var(--font-ui);
		outline: none;
	}

	.tag-input::placeholder { color: var(--fg-dim); }
	.tag-input:focus { border-color: var(--accent); }

	.search-btn {
		padding: 7px 16px;
		border-radius: 7px;
		background: var(--accent);
		color: var(--accent-text);
		border: none;
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
		font-family: var(--font-ui);
	}

	.search-btn:disabled { opacity: 0.5; cursor: not-allowed; }

	.discover-error { font-size: 11.5px; color: oklch(0.75 0.15 25); }

	.discover-results {
		flex: 1;
		overflow-y: auto;
		padding: 14px 18px;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(248px, 1fr));
		gap: 12px;
		align-content: start;
	}

	.discover-empty {
		grid-column: 1 / -1;
		text-align: center;
		color: var(--fg-dim);
		font-size: 12.5px;
		padding: 28px 0;
	}

	.hit-card {
		display: flex;
		flex-direction: column;
		gap: 7px;
		padding: 13px;
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 9px;
	}

	.hit-top { display: flex; align-items: center; gap: 9px; }
	.hit-id { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 1px; }
	.hit-name { font-size: 13px; font-weight: 600; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.hit-stranger { font-size: 9.5px; color: oklch(0.72 0.13 70); }

	.hit-follow {
		padding: 4px 12px;
		border-radius: 6px;
		background: var(--accent);
		color: var(--accent-text);
		border: none;
		font-size: 11.5px;
		font-weight: 600;
		cursor: pointer;
		font-family: var(--font-ui);
		flex-shrink: 0;
	}

	.hit-following { font-size: 11px; color: var(--fg-dim); flex-shrink: 0; }

	.hit-bio { font-size: 11.5px; color: var(--fg-muted); overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }

	.hit-fp {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 10px;
		color: var(--fg-dim);
		font-family: var(--font-mono);
	}

	.hit-fp-swatch { width: 10px; height: 10px; border-radius: 3px; flex-shrink: 0; }

	.hit-tags { display: flex; flex-wrap: wrap; gap: 4px; }

	.tag-ct { background: var(--accent-soft); color: var(--accent); border-color: color-mix(in oklch, var(--accent) 30%, transparent); }

	.hit-locked { display: inline-flex; align-items: center; font-size: 11px; color: var(--fg-dim); margin-top: 2px; }

	/* ── Context menu ────────────────────────────────────────────── */

	.ctx-backdrop {
		position: fixed;
		inset: 0;
		z-index: 999;
	}

	.ctx-menu {
		position: fixed;
		z-index: 1000;
		min-width: 160px;
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		border-radius: 8px;
		padding: 4px;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.4);
	}

	.ctx-item {
		display: flex;
		align-items: center;
		gap: 9px;
		width: 100%;
		padding: 7px 10px;
		background: transparent;
		border: none;
		border-radius: 5px;
		font-family: var(--font-ui);
		font-size: 12.5px;
		color: var(--fg);
		cursor: pointer;
		text-align: left;
	}

	.ctx-item:hover { background: var(--bg-elev2); }

	.ctx-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }
</style>
