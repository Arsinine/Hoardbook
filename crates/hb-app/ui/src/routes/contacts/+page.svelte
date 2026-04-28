<script lang="ts">
	import { pasteKey, follow, refreshContact, requestDownload, unfollowContact, setContactTags, getDirectory } from '$lib/api.js';
	import { save } from '@tauri-apps/plugin-dialog';
	import { contacts, identity, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import CollectionPanel from '$lib/components/CollectionPanel.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import type { CachedPeer, DirectoryPeer } from '$lib/types.js';
	import { onMount } from 'svelte';

	// Recommended directory
	let directory: DirectoryPeer[] = [];
	let directoryLoading = false;

	onMount(async () => {
		directoryLoading = true;
		try { directory = await getDirectory(); } catch { /* relay may be unreachable */ }
		finally { directoryLoading = false; }
	});

	$: recommendedPeers = directory.filter(d => !$contacts.some(c => c.hb_id === d.hb_id));

	async function handleFollowDirectory(peer: DirectoryPeer) {
		try {
			await follow(peer.hb_id);
			const fetched = await pasteKey(peer.hb_id);
			contacts.update(cs => cs.find(c => c.hb_id === peer.hb_id) ? cs : [...cs, fetched]);
			toast(`Following ${peer.profile?.display_name ?? peer.hb_id}`);
		} catch (e) { toast(String(e), 'error'); }
	}

	// Lookup state
	let input = '';
	let loading = false;
	let following = false;
	let result: CachedPeer | null = null;

	$: alreadyFollowed = $contacts.some((c) => c.hb_id === result?.hb_id);

	async function handleLookup() {
		const id = input.trim();
		if (!id) return;
		if (id === $identity?.hb_id) {
			toast("That's your own ID — you can't add yourself as a contact.", 'error');
			return;
		}
		loading = true;
		result = null;
		try {
			result = await pasteKey(id);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			loading = false;
		}
	}

	async function handleFollow() {
		if (!result) return;
		following = true;
		try {
			await follow(result.hb_id);
			contacts.update((cs) => {
				if (cs.find((c) => c.hb_id === result!.hb_id)) return cs;
				return [...cs, result!];
			});
			toast(`Following ${result.profile?.display_name ?? result.hb_id}`);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			following = false;
		}
	}

	// Contacts list state
	let expanded: string | null = null;
	let refreshing: string | null = null;
	let autoRefreshing: string | null = null;

	async function handleExpand(peer: CachedPeer) {
		const id = peer.hb_id;
		if (expanded === id) {
			expanded = null;
			return;
		}
		expanded = id;
		// Auto-refresh if the contact has no collections (might be stale cache).
		if (peer.collections.length === 0 && autoRefreshing !== id) {
			autoRefreshing = id;
			try {
				const updated = await refreshContact(id);
				contacts.update(cs => cs.map(c => c.hb_id === id ? { ...c, ...updated, local_tags: c.local_tags } : c));
			} catch { /* silent — don't nag if relay is unreachable */ }
			finally { autoRefreshing = null; }
		}
	}

	async function handleRefresh(hb_id: string) {
		refreshing = hb_id;
		try {
			const updated = await refreshContact(hb_id);
			contacts.update((cs) => cs.map((c) => (c.hb_id === hb_id ? updated : c)));
			toast('Contact refreshed');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			refreshing = null;
		}
	}

	async function handleUnfollow(hb_id: string) {
		try {
			await unfollowContact(hb_id);
			contacts.update((cs) => cs.filter((c) => c.hb_id !== hb_id));
			toast('Contact removed');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleDownload(e: CustomEvent<{ peerId: string; slug: string; path: string }>) {
		const filename = e.detail.path.split('/').pop() ?? e.detail.path;
		const savePath = await save({ defaultPath: filename });
		if (!savePath) return;
		const peer = $contacts.find((c) => c.hb_id === e.detail.peerId);
		try {
			await requestDownload(e.detail.peerId, peer?.node_addr ?? null, e.detail.slug, e.detail.path, savePath);
			toast(`Downloading to ${savePath}`);
		} catch (err) {
			toast(String(err), 'error');
		}
	}

	function shortId(hb_id: string) {
		return hb_id.length > 14 ? hb_id.slice(0, 8) + '…' + hb_id.slice(-4) : hb_id;
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') handleLookup();
	}

	function formatLastSeen(iso: string): string {
		const diff = Date.now() - new Date(iso).getTime();
		const mins = Math.floor(diff / 60_000);
		if (mins < 2) return 'just now';
		if (mins < 60) return `${mins}m ago`;
		const hrs = Math.floor(mins / 60);
		if (hrs < 24) return `${hrs}h ago`;
		return `${Math.floor(hrs / 24)}d ago`;
	}

	// Tag editing state
	let editingTagsFor: string | null = null;
	let tagInput = '';

	async function handleAddTag(hb_id: string, current_tags: string[]) {
		const tag = tagInput.trim();
		if (!tag || current_tags.includes(tag)) { tagInput = ''; return; }
		const newTags = [...current_tags, tag];
		tagInput = '';
		try {
			await setContactTags(hb_id, newTags);
			contacts.update(cs => cs.map(c => c.hb_id === hb_id ? { ...c, local_tags: newTags } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	async function handleRemoveTag(hb_id: string, current_tags: string[], tag: string) {
		const newTags = current_tags.filter(t => t !== tag);
		try {
			await setContactTags(hb_id, newTags);
			contacts.update(cs => cs.map(c => c.hb_id === hb_id ? { ...c, local_tags: newTags } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	// Filter by tag
	let filterTag = '';
	$: filteredContacts = filterTag
		? $contacts.filter(c => c.local_tags?.includes(filterTag))
		: $contacts;

	$: allTags = [...new Set($contacts.flatMap(c => c.local_tags ?? []))].sort();
</script>

<!-- TopBar -->
<div class="topbar">
	<div>
		<div class="topbar-title">Contacts</div>
		<div class="topbar-sub">
			{$contacts.length} followed peer{$contacts.length !== 1 ? 's' : ''} · {$contacts.filter(c => c.online).length} online
		</div>
	</div>
</div>

<div class="body">
	<!-- Lookup section -->
	<div class="lookup-section">
		<div class="lookup-label">Look up a peer by ID</div>
		<div class="search-row">
			<div class="search-input-wrap">
				<span class="search-icon">{@html icons.search}</span>
				<input
					class="search-input hb-mono"
					type="text"
					placeholder="hb1_…"
					bind:value={input}
					on:keydown={handleKeydown}
				/>
			</div>
			<button class="btn-primary" on:click={handleLookup} disabled={!input.trim() || loading}>
				{loading ? 'Looking up…' : 'Lookup'}
			</button>
		</div>

		{#if result}
			<div class="result">
				<div class="profile-card">
					<div class="profile-banner" />
					<div class="profile-inner">
						<div class="profile-top">
							<Avatar
								letter={(result.profile?.display_name ?? result.hb_id)[0].toUpperCase()}
								size={52}
								hue={avatarHue((result.profile?.display_name ?? result.hb_id)[0])}
							/>
							<div class="profile-name-col">
								<div class="name-row">
									<span class="peer-name">{result.profile?.display_name ?? 'Unknown'}</span>
									{#if result.online}
										<span class="pill pill-online"><span class="pill-dot" /> Online</span>
									{:else}
										<span class="pill pill-offline">Offline</span>
									{/if}
								</div>
								<span class="mono">{result.hb_id.slice(0, 18)}…{result.hb_id.slice(-4)}</span>
							</div>
							<div class="profile-actions">
								<button
									class="btn-primary btn-sm"
									on:click={handleFollow}
									disabled={alreadyFollowed || following}
								>
									{alreadyFollowed ? 'Following' : following ? '…' : 'Follow'}
								</button>
							</div>
						</div>

						{#if result.profile?.bio}
							<p class="peer-bio">{result.profile.bio}</p>
						{/if}

						{#if result.profile?.location || result.profile?.email || (result.profile?.social_links?.length ?? 0) > 0}
							<div class="contact-meta" style="margin-top: 4px">
								{#if result.profile?.location}
									<span class="meta-chip">{result.profile.location}</span>
								{/if}
								{#if result.profile?.email}
									<a class="meta-chip meta-link" href="mailto:{result.profile.email}">{result.profile.email}</a>
								{/if}
								{#each (result.profile?.social_links ?? []) as link}
									<span class="meta-chip meta-social">{link.platform}: {link.handle}</span>
								{/each}
							</div>
						{/if}

						<div class="peer-metrics">
							{#if result.profile?.est_size}
								<div class="metric">
									<div class="metric-label">Size</div>
									<div class="metric-val">{result.profile.est_size}</div>
								</div>
								<div class="metric-divider" />
							{/if}
							{#if result.collections.length > 0}
								<div class="metric">
									<div class="metric-label">Collections</div>
									<div class="metric-val">{result.collections.length}</div>
								</div>
							{/if}
							{#if result.profile?.since}
								<div class="metric-divider" />
								<div class="metric">
									<div class="metric-label">Since</div>
									<div class="metric-val">{result.profile.since}</div>
								</div>
							{/if}
							{#if result.profile?.languages?.length}
								<div class="metric-divider" />
								<div class="metric">
									<div class="metric-label">Languages</div>
									<div class="metric-val">{result.profile.languages.join(', ')}</div>
								</div>
							{/if}
						</div>
					</div>
				</div>

				{#if result.collections.length > 0}
					<div class="section-label">Public collections</div>
					<div class="coll-list-sm">
						{#each result.collections as col}
							<CollectionPanel collection={col} />
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</div>

	<!-- Recommended peers from relay directory -->
	{#if recommendedPeers.length > 0 || directoryLoading}
		<div class="section-divider">
			<div class="divider-line" />
			<span class="divider-label">Recommended</span>
			<div class="divider-line" />
		</div>
		{#if directoryLoading}
			<div class="empty">Loading recommended peers…</div>
		{:else}
			<div class="rec-list">
				{#each recommendedPeers as peer}
					{@const name = peer.profile?.display_name ?? peer.hb_id.slice(0, 12) + '…'}
					{@const initial = name[0]?.toUpperCase() ?? '?'}
					{@const hue = avatarHue(initial)}
					<div class="rec-card">
						<Avatar letter={initial} size={38} {hue} />
						<div class="rec-info">
							<div class="name-row">
								<span class="peer-name">{name}</span>
							</div>
							{#if peer.profile?.bio}
								<p class="bio">{peer.profile.bio}</p>
							{/if}
							{#if peer.profile?.location}
								<span class="rec-location">{peer.profile.location}</span>
							{/if}
						</div>
						<button class="btn-primary btn-sm" on:click={() => handleFollowDirectory(peer)}>
							Follow
						</button>
					</div>
				{/each}
			</div>
		{/if}
	{/if}

	<!-- Divider + tag filter -->
	<div class="section-divider">
		<div class="divider-line" />
		<span class="divider-label">Following ({$contacts.length})</span>
		<div class="divider-line" />
	</div>

	{#if allTags.length > 0}
		<div class="tag-filter-row">
			<button class="filter-tag" class:filter-tag-active={!filterTag} on:click={() => filterTag = ''}>All</button>
			{#each allTags as tag}
				<button class="filter-tag" class:filter-tag-active={filterTag === tag} on:click={() => filterTag = filterTag === tag ? '' : tag}>{tag}</button>
			{/each}
		</div>
	{/if}

	<!-- Contacts list -->
	{#if $contacts.length === 0}
		<div class="empty">No contacts yet. Look up a peer above and follow them.</div>
	{:else if filteredContacts.length === 0}
		<div class="empty">No contacts with tag "{filterTag}".</div>
	{:else}
		<div class="contact-list">
			{#each filteredContacts as peer}
				{@const name = peer.profile?.display_name ?? 'Unknown'}
				{@const initial = name[0]?.toUpperCase() ?? '?'}
				{@const hue = avatarHue(initial)}
				<div class="contact-block">
					<div class="contact-card">
						<Avatar letter={initial} size={42} {hue} />
						<div class="contact-info">
							<div class="name-row">
								<span class="peer-name">{name}</span>
								{#if peer.online}
									<span class="pill pill-online"><span class="pill-dot" /> Online</span>
								{:else}
									<span class="pill pill-offline">Offline</span>
								{/if}
								<span class="last-seen">seen {peer.last_fetched ? formatLastSeen(peer.last_fetched) : 'never'}</span>
							</div>
							<div class="mono">{shortId(peer.hb_id)}</div>
							{#if peer.profile?.bio}
								<p class="bio">{peer.profile.bio}</p>
							{/if}

							<!-- Location / email / social links -->
							{#if peer.profile?.location || peer.profile?.email || (peer.profile?.social_links?.length ?? 0) > 0}
								<div class="contact-meta">
									{#if peer.profile?.location}
										<span class="meta-chip">{@html icons.location ?? '📍'} {peer.profile.location}</span>
									{/if}
									{#if peer.profile?.email}
										<a class="meta-chip meta-link" href="mailto:{peer.profile.email}">{peer.profile.email}</a>
									{/if}
									{#each (peer.profile?.social_links ?? []) as link}
										<span class="meta-chip meta-social">{link.platform}: {link.handle}</span>
									{/each}
								</div>
							{/if}

							<!-- Local tags -->
							<div class="tag-row">
								{#each (peer.local_tags ?? []) as tag}
									<span class="local-tag">
										{tag}
										<button class="tag-x" on:click={() => handleRemoveTag(peer.hb_id, peer.local_tags ?? [], tag)}>×</button>
									</span>
								{/each}
								{#if editingTagsFor === peer.hb_id}
									<input
										class="tag-input"
										type="text"
										placeholder="tag…"
										bind:value={tagInput}
										on:keydown={(e) => {
											if (e.key === 'Enter' || e.key === ',') { e.preventDefault(); handleAddTag(peer.hb_id, peer.local_tags ?? []); }
											if (e.key === 'Escape') { editingTagsFor = null; tagInput = ''; }
										}}
										on:blur={() => { editingTagsFor = null; tagInput = ''; }}
									/>
								{:else}
									<button class="tag-add-btn" on:click={() => { editingTagsFor = peer.hb_id; tagInput = ''; }}>+ tag</button>
								{/if}
							</div>

							<div class="contact-footer">
								{#if peer.profile?.est_size}
									<span class="tnum">~{peer.profile.est_size}</span>
								{/if}
								{#if peer.collections.length > 0}
									<span class="dot">·</span>
									<span>{peer.collections.length} collection{peer.collections.length !== 1 ? 's' : ''}</span>
								{/if}
								<div style="flex:1" />
								<button
									class="btn-ghost btn-sm btn-icon"
									on:click={() => handleRefresh(peer.hb_id)}
									disabled={refreshing === peer.hb_id}
								>
									<span>{@html icons.refresh}</span>
									{refreshing === peer.hb_id ? '…' : 'Refresh'}
								</button>
								<button class="btn-ghost btn-sm btn-danger" on:click={() => handleUnfollow(peer.hb_id)}>
									Unfollow
								</button>
								<button
									class="btn-default btn-sm"
									on:click={() => handleExpand(peer)}
								>
									{expanded === peer.hb_id ? 'Hide' : 'View'} collections
								</button>
							</div>
						</div>
					</div>

					{#if expanded === peer.hb_id}
						<div class="collections-indent">
							{#if autoRefreshing === peer.hb_id}
								<p class="no-coll">Checking for collections…</p>
							{:else if peer.collections.length === 0}
								<p class="no-coll">No collections published.</p>
							{:else}
								{#each peer.collections as col}
									<CollectionPanel collection={col} peerId={peer.hb_id} on:download={handleDownload} />
								{/each}
							{/if}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.topbar {
		padding: 16px 24px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		background: var(--bg);
		flex-shrink: 0;
	}
	.topbar-title { font-size: 17px; font-weight: 600; letter-spacing: -0.3px; }
	.topbar-sub { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }

	.body { padding: 24px; overflow-y: auto; flex: 1; max-width: 720px; display: flex; flex-direction: column; gap: 0; }

	/* Lookup */
	.lookup-section { margin-bottom: 20px; }

	.lookup-label {
		font-size: 10.5px;
		color: var(--fg-dim);
		text-transform: uppercase;
		letter-spacing: 1.2px;
		font-weight: 600;
		margin-bottom: 10px;
	}

	.search-row { display: flex; gap: 8px; margin-bottom: 16px; }

	.search-input-wrap {
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

	.search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }

	.search-input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 13px;
		color: var(--fg);
		min-width: 0;
	}
	.search-input::placeholder { color: var(--fg-dim); }
	.hb-mono { font-family: var(--font-mono); }

	.result { display: flex; flex-direction: column; gap: 12px; }

	/* Profile card (browse style) */
	.profile-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		overflow: hidden;
	}

	.profile-banner {
		height: 52px;
		background: linear-gradient(135deg, oklch(0.30 0.10 280) 0%, oklch(0.25 0.12 320) 100%);
		border-bottom: 1px solid var(--border);
	}

	.profile-inner {
		padding: 0 16px 16px;
		margin-top: -26px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.profile-top { display: flex; gap: 12px; align-items: flex-end; }

	.profile-name-col { flex: 1; min-width: 0; padding-bottom: 4px; }

	.name-row { display: flex; gap: 8px; align-items: center; margin-bottom: 3px; flex-wrap: wrap; }

	.peer-name { font-weight: 600; font-size: 15px; letter-spacing: -0.2px; }

	.mono { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	.profile-actions { display: flex; gap: 8px; padding-bottom: 4px; }

	.peer-bio { font-size: 13px; color: var(--fg); line-height: 1.55; margin: 0; }

	.peer-metrics { display: flex; gap: 14px; font-size: 12px; align-items: center; flex-wrap: wrap; }

	.metric { display: flex; flex-direction: column; gap: 2px; }

	.metric-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 0.9px; font-weight: 600;
	}

	.metric-val { font-size: 14px; font-weight: 600; color: var(--fg); font-feature-settings: 'tnum'; }

	.metric-divider { width: 1px; align-self: stretch; background: var(--divider); }

	.section-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
	}

	.coll-list-sm { display: flex; flex-direction: column; gap: 8px; }

	/* Divider */
	.section-divider {
		display: flex;
		align-items: center;
		gap: 12px;
		margin: 4px 0 20px;
	}

	.divider-line { flex: 1; height: 1px; background: var(--divider); }

	.divider-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
		white-space: nowrap;
	}

	/* Recommended list */
	.rec-list { display: flex; flex-direction: column; gap: 8px; margin-bottom: 8px; }

	.rec-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 12px 14px;
		display: flex;
		gap: 12px;
		align-items: center;
	}

	.rec-info { flex: 1; min-width: 0; }

	.rec-location { font-size: 11px; color: var(--fg-dim); margin-top: 2px; display: block; }

	/* Contacts list */
	.empty { color: var(--fg-dim); font-size: 13px; padding: 16px 0; }

	.contact-list { display: flex; flex-direction: column; gap: 12px; }

	.contact-block { display: flex; flex-direction: column; gap: 8px; }

	.contact-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 14px;
		display: flex;
		gap: 14px;
		align-items: flex-start;
	}

	.contact-info { flex: 1; min-width: 0; }

	.last-seen { font-size: 11px; color: var(--fg-dim); margin-left: auto; }

	.bio { font-size: 12.5px; color: var(--fg-muted); margin-top: 6px; line-height: 1.5; }

	.contact-meta { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }

	.meta-chip {
		font-size: 11px;
		color: var(--fg-muted);
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 1px 7px;
		white-space: nowrap;
	}

	.meta-link {
		color: var(--accent);
		text-decoration: none;
	}
	.meta-link:hover { text-decoration: underline; }

	.meta-social { font-family: var(--font-mono); font-size: 10.5px; }

	.contact-footer {
		display: flex; gap: 8px; align-items: center;
		margin-top: 8px; font-size: 11.5px; color: var(--fg-muted);
	}

	.tnum { font-feature-settings: 'tnum'; }
	.dot { color: var(--fg-dim); }

	/* Tag filter bar */
	.tag-filter-row { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 14px; }
	.filter-tag {
		padding: 3px 10px; font-size: 11px; font-weight: 500;
		border: 1px solid var(--border); border-radius: 999px;
		background: transparent; color: var(--fg-muted); cursor: pointer;
		font-family: var(--font-ui);
	}
	.filter-tag:hover { border-color: var(--accent); color: var(--accent); }
	.filter-tag-active { background: var(--accent-soft); border-color: var(--accent); color: var(--accent); }

	/* Local tags on contact cards */
	.tag-row { display: flex; flex-wrap: wrap; gap: 4px; margin: 5px 0 2px; align-items: center; min-height: 22px; }
	.local-tag {
		display: inline-flex; align-items: center; gap: 3px;
		padding: 1px 6px 1px 8px; border-radius: 4px; font-size: 11px; font-weight: 500;
		background: var(--bg-elev2); border: 1px solid var(--border); color: var(--fg-muted);
	}
	.tag-x {
		background: none; border: none; cursor: pointer; color: var(--fg-dim);
		font-size: 13px; line-height: 1; padding: 0; display: flex; align-items: center;
	}
	.tag-x:hover { color: var(--fg); }
	.tag-add-btn {
		font-size: 11px; color: var(--fg-dim); background: transparent; border: 1px dashed var(--border);
		border-radius: 4px; padding: 1px 7px; cursor: pointer; font-family: var(--font-ui);
	}
	.tag-add-btn:hover { border-color: var(--accent); color: var(--accent); }
	.tag-input {
		font-size: 11px; background: var(--bg-input); border: 1px solid var(--accent);
		border-radius: 4px; padding: 1px 7px; outline: none; color: var(--fg);
		min-width: 60px; font-family: var(--font-ui);
	}

	.collections-indent { padding-left: 56px; display: flex; flex-direction: column; gap: 8px; }

	.no-coll { font-size: 12px; color: var(--fg-dim); }

	/* Pills */
	.pill {
		display: inline-flex; align-items: center; gap: 5px;
		font-size: 10.5px; font-weight: 500;
		padding: 2px 8px; border-radius: 999px;
	}
	.pill-dot { width: 5px; height: 5px; border-radius: 50%; }
	.pill-online {
		color: var(--online);
		background: color-mix(in oklch, var(--online) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--online) 20%, transparent);
	}
	.pill-online .pill-dot { background: var(--online); }
	.pill-offline {
		color: var(--fg-muted);
		background: color-mix(in oklch, var(--fg-muted) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--fg-muted) 20%, transparent);
	}

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
	.btn-ghost:disabled { opacity: 0.5; cursor: not-allowed; }
	.btn-sm { padding: 5px 11px; font-size: 12px; }
	.btn-icon { gap: 4px; }
	.btn-danger { color: var(--red, #e05c5c); }
	.btn-danger:hover { background: color-mix(in oklch, var(--red, #e05c5c) 10%, transparent); }
</style>
