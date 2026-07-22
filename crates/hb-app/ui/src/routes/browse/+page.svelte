<script lang="ts">
	import { contacts, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import { refreshContact, importManifest, requestManifest } from '$lib/api.js';
	import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
	import { page } from '$app/stores';
	import Avatar from '$lib/components/Avatar.svelte';
	import FeatureTooltip from '$lib/components/FeatureTooltip.svelte';
	import { collectionAvailability, peerAccessBadge, peerFromQuery, paywallTeaser, importedManifestNote, arrangeItems, fileTypesPresent, type BrowseViewMode, type BrowseSortKey, type BrowseSortDir } from '$lib/browse-view.js';
	import type { CachedPeer, Collection, DirectoryItem } from '$lib/types.js';

	type BcItem =
		| { label: string; kind: 'contact' }
		| { label: string; kind: 'collection' }
		| { label: string; kind: 'folder'; index: number };

	let search = $state('');
	let selectedPeer = $state<CachedPeer | null>(null);
	let selectedCollection = $state<Collection | null>(null);
	let folderStack: { name: string; items: DirectoryItem[] }[] = $state([]);
	// devtest #3/#4: a keyed contact's collections are cached at add-time; a flaky fetch then leaves
	// them empty forever, so browsing shows nothing. Re-pull live when the peer is selected.
	let loadingListings = $state(false);

	// devtest v0.12.4 #4: file-view controls. viewMode/sort are sticky preferences; the type filter +
	// in-collection search reset on navigation (a stale filter would hide a folder's whole content).
	let viewMode: BrowseViewMode = $state('details');
	let sortKey: BrowseSortKey = $state('name');
	let sortDir: BrowseSortDir = $state('asc');
	let fileSearch = $state('');
	let activeTypes: string[] = $state([]);

	function resetFileFilters() {
		fileSearch = '';
		activeTypes = [];
	}

	function toggleType(t: string) {
		activeTypes = activeTypes.includes(t) ? activeTypes.filter((x) => x !== t) : [...activeTypes, t];
	}

	function peerName(peer: CachedPeer): string {
		// A legacy/adversarial teaser can carry display_name: "" (R1 only guards publish) — `??` would
		// not fall back to a literal empty string, showing a blank name; `||` does.
		return peer.profile?.display_name || peer.npub.slice(0, 10) + '…';
	}

	function peerInitial(peer: CachedPeer): string {
		return (peer.profile?.display_name?.[0] ?? peer.npub[0]).toUpperCase();
	}

	async function selectPeer(peer: CachedPeer) {
		selectedPeer = peer;
		selectedCollection = null;
		folderStack = [];
		resetFileFilters();
		// devtest #3/#4: for a keyed contact, re-fetch listings live so a browse-key that arrived
		// after (or a listing fetch that hiccuped at) add-time actually surfaces their collections.
		// Bare (keyless) contacts have nothing to fetch — skip. Cached view stays if the fetch fails.
		if (!peer.browse_key_hex) return;
		loadingListings = true;
		try {
			const updated = await refreshContact(peer.npub);
			contacts.update(cs => cs.map(c => c.npub === updated.npub ? { ...updated, local_tags: c.local_tags } : c));
			// Only replace the view if the user hasn't navigated to a different peer meanwhile.
			if (selectedPeer?.npub === updated.npub) selectedPeer = updated;
		} catch {
			/* keep the cached view — offline / relay hiccup shouldn't blank the panel */
		} finally {
			loadingListings = false;
		}
	}

	// M15 W4: resolve a `/browse?peer=<npub>` deep-link (from the Contacts "Browse" button) THROUGH
	// selectPeer, so the keyed-contact live-refetch (devtest #3/#4) fires by construction. Guarded so
	// it runs once per distinct param (and waits for contacts to load — peerFromQuery returns null
	// until the match exists).
	let lastDeepLinked = '';
	$effect(() => {
		const npub = $page.url.searchParams.get('peer') ?? '';
		if (!npub || npub === lastDeepLinked) return;
		const peer = peerFromQuery($page.url.searchParams, $contacts);
		if (peer) {
			lastDeepLinked = npub;
			selectPeer(peer);
		}
	});

	function selectCollection(col: Collection) {
		selectedCollection = col;
		folderStack = [];
		resetFileFilters();
	}

	// M16 W4: import a full-listing manifest the user received out of band, upgrading a truncated
	// paywall teaser to the whole tree. The backend verifies the manifest author against this peer's
	// npub before decrypting; on success the fade lifts (`truncated` cleared ⇒ `paywallTeaser` → null).
	let importingManifest = $state(false);
	let pasteOpen = $state(false);
	let pasteText = $state('');
	let askingOwner = $state(false);

	// M16 W4: the primary "get the rest" affordance — DM the owner asking for the full list. The owner
	// decides whether to export + ticket a manifest (Hoardbook never auto-produces one; MASCARA_SPEC Q1).
	async function handleAskOwner() {
		if (!selectedPeer || !selectedCollection) return;
		askingOwner = true;
		try {
			await requestManifest(selectedPeer.npub, selectedCollection.slug, selectedCollection.snapshot_fingerprint ?? '', selectedCollection.teaser_event_id);
			toast('Asked the owner for the full list');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			askingOwner = false;
		}
	}

	async function handleImportManifest(source: { path?: string; pasted?: string }) {
		if (!selectedPeer || !selectedCollection) return;
		const targetNpub = selectedPeer.npub;
		const targetSlug = selectedCollection.slug;
		importingManifest = true;
		try {
			const result = await importManifest(targetNpub, targetSlug, source, selectedCollection.snapshot_fingerprint);
			const full = result.collection;
			// Swap the truncated collection for the full tree, in the view and the in-memory contact.
			selectedPeer = {
				...selectedPeer,
				collections: selectedPeer.collections.map((c) => (c.slug === result.slug ? full : c)),
			};
			selectedCollection = full;
			folderStack = [];
			if (result.stale) {
				toast('Imported an older version of this list — ask the owner for a fresh manifest.', 'error');
			} else {
				toast('Full manifest imported');
			}
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			importingManifest = false;
			pasteOpen = false;
			pasteText = '';
		}
	}

	async function pickManifestFile() {
		const path = await openFileDialog({
			multiple: false,
			filters: [{ name: 'Hoardbook manifest', extensions: ['hbmanifest'] }],
		});
		if (typeof path === 'string') await handleImportManifest({ path });
	}

	function enterFolder(item: DirectoryItem) {
		folderStack = [...folderStack, { name: item.name, items: item.children }];
		resetFileFilters();
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
		resetFileFilters();
	}

	function fmtBytes(bytes: number): string {
		if (bytes > 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
		if (bytes > 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
		if (bytes > 1e3) return (bytes / 1e3).toFixed(0) + ' KB';
		return bytes + ' B';
	}

	// Build the relative path for a file within the collection.
	// devtest #9: no right-click "Copy path" — Hoardbook shows metadata only and moves no files, so
	// there's nothing to copy a usable path to; the context menu is removed entirely.

	// §6 Discovery moved to Contacts (devtest 2026-06-25 #6). Browse is now purely "browse a contact's
	// collections" — pick someone from the People list on the left.

	let filteredContacts = $derived($contacts
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
		}));
	let currentItems = $derived(folderStack.length > 0
		? folderStack[folderStack.length - 1].items
		: (selectedCollection?.listing ?? []));
	// devtest v0.12.4 #4: the distinct file types in the current folder feed the filter chips; the
	// arranged list applies search + type filter + sort (folders-first) — all in the tested seam.
	let availableTypes = $derived(fileTypesPresent(currentItems));
	let sortedItems = $derived(arrangeItems(currentItems, { search: fileSearch, types: activeTypes, sortKey, sortDir }));
	// devtest #7 / M16 W3: a peer's collection published as a truncated paywall teaser (too large to
	// publish whole). Shown only at the collection root, where the dropped tail makes the fade honest;
	// a collection the browser upgraded to the full tree from a big relay has `truncated` cleared, so
	// `paywallTeaser` returns null and the full tree renders (no fade).
	let paywall = $derived(folderStack.length > 0 ? null : paywallTeaser(selectedCollection));
	let breadcrumbs = $derived<BcItem[]>([
		...(selectedPeer ? [{ label: peerName(selectedPeer), kind: 'contact' as const }] : []),
		...(selectedCollection ? [{ label: selectedCollection.path_alias, kind: 'collection' as const }] : []),
		...folderStack.map((f, i) => ({ label: f.name, kind: 'folder' as const, index: i })),
	]);
	// Feature-tooltip anchor data (HOARDBOOK_SPEC §8).
	let peerWillingTo = $derived(selectedPeer?.profile?.willing_to ?? []);
	// A peer followed by bare npub (no share code) has sealed listings — they can't be decrypted.
	let listingsLocked = $derived(!!selectedPeer && !selectedPeer.browse_key_hex && selectedPeer.collections.length === 0);
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
					{@const badge = peerAccessBadge(peer)}
					<button
						class="contact-row"
						class:contact-selected={selectedPeer?.npub === peer.npub}
						onclick={() => selectPeer(peer)}
					>
						<div class="avatar-wrap">
							<Avatar {letter} size={28} {hue} picture={peer.profile?.picture} />
							<!-- devtest v0.12.1 #3: the browse-key lock/unlock icon overlays the avatar's top-right
							     (the online dot owns the bottom-right); the inline text badge is gone. -->
							<span class="access-lock" class:locked={badge.locked} title={badge.hint || badge.label}>{badge.icon}</span>
							{#if peer.online}
								<span class="online-dot"></span>
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
			<!-- Browse = view a contact's collections. Finding/adding people (lookup + Discover
			     hoarders) now lives on Contacts (devtest 2026-06-25 #6). -->
			<div class="empty-state">
				<div class="empty-icon">{@html icons.folder}</div>
				<div class="empty-label">Select a contact to browse their collections</div>
				{#if $contacts.length === 0}
					<a class="empty-cta" href="/contacts">Find hoarders in Contacts →</a>
				{/if}
			</div>
		{:else}
			<!-- Breadcrumb -->
			<div class="breadcrumb">
				{#each breadcrumbs as bc, i}
					{#if i > 0}
						<span class="bc-sep">{@html icons.chevronRight}</span>
					{/if}
					{#if i < breadcrumbs.length - 1}
						<button class="bc-btn" onclick={() => navigateBc(bc)}>{bc.label}</button>
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
				{#if loadingListings && selectedPeer.collections.length === 0}
					<div class="empty-state">
						<div class="empty-icon">{@html icons.folder}</div>
						<div class="empty-label">Loading collections…</div>
					</div>
				{:else if listingsLocked}
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
							<button class="col-card" onclick={() => selectCollection(col)}>
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
					<!-- devtest v0.12.4 #4: file-view controls — Details/Folders toggle · sort · type filter · search. -->
					<div class="file-toolbar">
						<div class="view-toggle" role="group" aria-label="View mode">
							<button type="button" aria-pressed={viewMode === 'details'} onclick={() => (viewMode = 'details')}>Details</button>
							<button type="button" aria-pressed={viewMode === 'folders'} onclick={() => (viewMode = 'folders')}>Folders</button>
						</div>
						<div class="sort-control">
							<select class="sort-select" bind:value={sortKey} aria-label="Sort by">
								<option value="name">Name</option>
								<option value="size">Size</option>
								<option value="type">Type</option>
							</select>
							<button type="button" class="sort-dir" onclick={() => (sortDir = sortDir === 'asc' ? 'desc' : 'asc')} title="Sort direction" aria-label="Toggle sort direction">
								{sortDir === 'asc' ? '↑' : '↓'}
							</button>
						</div>
						<div class="file-search">
							<span class="search-icon">{@html icons.search}</span>
							<input placeholder="Search this collection…" bind:value={fileSearch} aria-label="Search items" />
						</div>
					</div>
					{#if availableTypes.length > 0}
						<div class="type-filter">
							<button type="button" class="type-chip" class:type-chip-active={activeTypes.length === 0} onclick={() => (activeTypes = [])}>All types</button>
							{#each availableTypes as t (t)}
								<button type="button" class="type-chip" class:type-chip-active={activeTypes.includes(t)} onclick={() => toggleType(t)}>{t}</button>
							{/each}
						</div>
					{/if}
					{#if sortedItems.length === 0}
						<div class="empty-state">
							<div class="empty-icon">{@html icons.folder}</div>
							<div class="empty-label">{fileSearch.trim() || activeTypes.length > 0 ? 'No items match your filters' : 'Empty folder'}</div>
						</div>
					{:else if viewMode === 'details'}
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
									onclick={() => { if (item.item_type === 'Folder') enterFolder(item); }}
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
					{:else}
						<!-- Folders (tile) view — the same metadata as Details, laid out as large icons. -->
						<div class="item-grid">
							{#each sortedItems as item (item.name)}
								<button
									class="item-tile"
									class:file-folder={item.item_type === 'Folder'}
									class:file-leaf={item.item_type === 'File'}
									onclick={() => { if (item.item_type === 'Folder') enterFolder(item); }}
								>
									<div class="item-tile-icon">{@html item.item_type === 'Folder' ? icons.folder : icons.file}</div>
									<div class="item-tile-name">{item.name}</div>
									<div class="item-tile-meta">
										{item.item_type === 'Folder' ? 'Folder' : (item.format ?? 'File')}{#if item.size} · {item.size}{/if}
									</div>
								</button>
							{/each}
						</div>
					{/if}
					{#if paywall}
						<!-- devtest #7: paywall fade — the owner published only a preview of a too-large collection. -->
						<div class="paywall">
							<div class="paywall-fade"></div>
							<div class="paywall-note">
								<span class="paywall-lock">🔒</span>
								<div>
									<div class="paywall-title">{paywall.hidden.toLocaleString()} more item{paywall.hidden !== 1 ? 's' : ''} hidden</div>
									<div class="paywall-sub">Showing {paywall.shown.toLocaleString()} of {paywall.total.toLocaleString()} — this collection is too large to publish in full.</div>
									<!-- M16 W4: the "get the rest" affordance — import a manifest file the owner handed over
									     (out of band, via Mascara). No Download button (MAS-INV-5): Hoardbook moves no files. -->
									<div class="paywall-actions">
										<button class="btn-primary btn-sm" onclick={handleAskOwner} disabled={askingOwner}>Ask the owner for the full list</button>
										<button class="btn-ghost btn-sm" onclick={pickManifestFile} disabled={importingManifest}>Import a manifest file you received</button>
										<button class="btn-ghost btn-sm" onclick={() => (pasteOpen = !pasteOpen)}>or paste it</button>
									</div>
									{#if pasteOpen}
										<textarea class="hb-input hb-mono paywall-paste" bind:value={pasteText} placeholder="Paste the .hbmanifest text or its base64 here"></textarea>
										<button class="btn-primary btn-sm" disabled={importingManifest || !pasteText.trim()} onclick={() => handleImportManifest({ pasted: pasteText })}>Import from text</button>
									{/if}
								</div>
							</div>
						</div>
					{/if}
					{#if folderStack.length === 0 && importedManifestNote(selectedCollection)}
						<div class="imported-note"><span>{importedManifestNote(selectedCollection)}</span></div>
					{/if}
					{#if collectionAvailability(selectedCollection)}
						<div class="kofn-note">
							<span>{collectionAvailability(selectedCollection)}</span>
							<FeatureTooltip key="k-of-n-folders" />
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

	/* devtest v0.12.1 #3: browse-key lock/unlock badge, overlapping the avatar's top-right corner
	   (mirrors the bottom-right online dot). Shown for both states (🔓 browseable / 🔒 key needed). */
	.access-lock {
		position: absolute;
		top: -6px;
		right: -6px;
		font-size: 13px;
		line-height: 1;
		padding: 1px 2px;
		border-radius: 999px;
		background: var(--bg);
		box-shadow: 0 0 0 1px var(--border);
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

	.empty-cta {
		font-size: 12px;
		color: var(--accent);
		text-decoration: none;
		margin-top: 4px;
	}
	.empty-cta:hover { text-decoration: underline; }

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

	/* K-of-N availability note (M13 HANDOVER gap #5) */
	.kofn-note {
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 10px 16px;
		border-top: 1px solid var(--divider);
		font-size: 11.5px;
		color: var(--fg-dim);
		flex-shrink: 0;
	}

	/* devtest #7: paywall teaser — a gradient fade over the last rows + a "N more hidden" note. */
	.paywall { position: relative; flex-shrink: 0; }
	.paywall-fade {
		height: 56px;
		margin-top: -56px;
		pointer-events: none;
		background: linear-gradient(to bottom, transparent, var(--bg) 92%);
	}
	.paywall-note {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 16px 16px;
		color: var(--fg-muted);
	}
	.paywall-lock { font-size: 16px; flex-shrink: 0; }
	.paywall-title { font-size: 12.5px; font-weight: 600; color: var(--fg); }
	.paywall-sub { font-size: 11.5px; color: var(--fg-dim); margin-top: 1px; }

	/* M16 W4: the "get the rest" affordances inside the paywall note. */
	.paywall-actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; }
	.paywall-paste { display: block; width: 100%; margin-top: 6px; min-height: 52px; resize: vertical; }
	.imported-note {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 16px;
		font-size: 11.5px;
		color: var(--online);
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

	/* devtest v0.12.4 #4: file-view toolbar (Details/Folders · sort · search) + type-filter chips. */
	.file-toolbar {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		padding: 10px 14px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}
	.view-toggle {
		display: flex;
		border: 1px solid var(--border);
		border-radius: 7px;
		overflow: hidden;
		flex-shrink: 0;
	}
	.view-toggle button {
		padding: 5px 11px;
		font-size: 12px;
		font-weight: 600;
		background: transparent;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		font-family: var(--font-ui);
	}
	.view-toggle button[aria-pressed='true'] { background: var(--accent-soft); color: var(--accent); }

	.sort-control { display: flex; align-items: center; gap: 4px; flex-shrink: 0; }
	.sort-select {
		height: 28px; padding: 0 6px; border-radius: 6px; font-size: 12px;
		background: var(--bg-input); border: 1px solid var(--border); color: var(--fg);
		font-family: var(--font-ui); cursor: pointer;
	}
	.sort-dir {
		width: 28px; height: 28px; flex-shrink: 0;
		display: flex; align-items: center; justify-content: center;
		background: var(--bg-input); border: 1px solid var(--border); border-radius: 6px;
		color: var(--fg-muted); cursor: pointer; font-size: 13px; line-height: 1;
	}
	.sort-dir:hover { color: var(--fg); border-color: var(--border-strong); }

	.file-search {
		display: flex; align-items: center; gap: 6px;
		flex: 1; min-width: 140px; max-width: 280px;
		padding: 0 10px; height: 28px;
		background: var(--bg-input); border: 1px solid var(--border); border-radius: 7px;
	}
	.file-search .search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }
	.file-search input {
		flex: 1; min-width: 0; background: transparent; border: none; outline: none;
		font-size: 12px; color: var(--fg); font-family: var(--font-ui);
	}
	.file-search input::placeholder { color: var(--fg-dim); }

	.type-filter {
		display: flex; flex-wrap: wrap; gap: 6px;
		padding: 8px 14px; border-bottom: 1px solid var(--divider); flex-shrink: 0;
	}
	.type-chip {
		padding: 2px 10px; font-size: 11px; font-weight: 500;
		border: 1px solid var(--border); border-radius: 999px;
		background: transparent; color: var(--fg-muted); cursor: pointer;
		font-family: var(--font-ui);
	}
	.type-chip:hover { border-color: var(--accent); color: var(--accent); }
	.type-chip-active { background: var(--accent-soft); border-color: var(--accent); color: var(--accent); }

	/* Folders (tile) view — the same metadata as Details, laid out as large icons. */
	.item-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
		gap: 10px;
		padding: 14px;
		align-content: start;
	}
	.item-tile {
		display: flex; flex-direction: column; align-items: center; gap: 4px;
		padding: 14px 10px;
		background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 8px;
		text-align: center; color: inherit; font-family: inherit;
		cursor: default; min-width: 0;
	}
	.item-tile.file-folder { cursor: pointer; }
	.item-tile:hover { background: var(--bg-elev2); border-color: var(--border-strong); }
	.item-tile-icon { color: var(--fg-muted); display: flex; transform: scale(1.4); margin: 4px 0 8px; }
	.item-tile.file-folder .item-tile-icon { color: var(--accent); }
	.item-tile-name {
		font-size: 12px; color: var(--fg); font-weight: 500;
		overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%;
	}
	.item-tile-meta { font-size: 10px; color: var(--fg-dim); }

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

</style>
