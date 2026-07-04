<script lang="ts">
	import { pasteKey, follow, refreshContact, unfollowContact, setContactTags, groupsGet, groupsCreate, contactUpdateGroups, groupsSetTrusted, browsePrivateCollections, onlineCount, relayStatus, searchPeers, getContacts, type OnlineCount, type RelayHealth, type PeerSearchHit } from '$lib/api.js';
	import { contacts, identity, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import CollectionPanel from '$lib/components/CollectionPanel.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import FeatureTooltip from '$lib/components/FeatureTooltip.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';
	import CreateGroupDialog from '$lib/components/CreateGroupDialog.svelte';
	import AddContactDialog from '$lib/components/AddContactDialog.svelte';
	import type { CachedPeer, Collection, Group } from '$lib/types.js';
	import { renderFingerprint } from '$lib/identity-display.js';
	import { contactDisplayName } from '$lib/contact-display.js';
	import { DISCOVER_CONTENT_TYPES, parseTagInput, canSearch, toggleContentType } from '$lib/discover-view.js';
	import { NOT_DRM_NOTE, isTrusted } from '$lib/private-collections-view.js';
	import { onlineChipView } from '$lib/online-chip.js';
	import { relayWhyHint } from '$lib/relay-health.js';
	import { ONLINE_POLL_VISIBLE_MS } from '$lib/poll-lifecycle.js';
	import { onMount, onDestroy } from 'svelte';

	// ── "🟢 N hoarders online" chip (M9) — relay-derived, no telemetry. **Always shown** (the Settings
	//    hide-toggle was removed); lives here on Contacts. Best-effort + cached on the backend; polled on
	//    a slow tick (L4-budgeted); shows "–" while the count is unknown (m4).
	let onlineData: OnlineCount | null = null;
	let relayHealth: RelayHealth[] = [];
	$: chip = onlineChipView(onlineData, true);
	// M12 W1 Decision D: when the chip can't show a number, say *why* (which relays are unreachable).
	$: whyHint = chip.unknown ? relayWhyHint(relayHealth) : '';
	let onlinePollTimer: ReturnType<typeof setInterval> | undefined;
	async function refreshOnline() {
		// Decision B: don't poll the relays while the window is hidden (tray/minimized).
		if (document.hidden) return;
		try { onlineData = await onlineCount(); } catch { /* keep last value; chip shows "–" */ }
		// Drive the "why" hint only when the count is unknown (cheap, status-only read).
		try { relayHealth = await relayStatus(); } catch { /* leave last health */ }
	}
	onDestroy(() => { if (onlinePollTimer) clearInterval(onlinePollTimer); });


	// Groups state
	let groups: Group[] = [];

	async function loadGroups() {
		try { groups = await groupsGet(); } catch { /* non-fatal */ }
	}

	// M10: Private collections trusted peers have sealed to me, keyed by author npub. A non-trusted
	// viewer simply has no entry — there is no locked-teaser hint.
	let privateByAuthor: Record<string, Collection[]> = {};

	async function loadPrivate() {
		try {
			const groups = await browsePrivateCollections();
			const map: Record<string, Collection[]> = {};
			for (const g of groups) map[g.npub] = g.collections;
			privateByAuthor = map;
		} catch { /* non-fatal — relays may be unreachable */ }
	}

	// Mark a contact group trusted/untrusted (M10). Trusted groups receive every Private collection
	// on the next publish; un-trusting revokes on the next republish only (not retroactively).
	async function toggleTrusted(g: Group) {
		try {
			await groupsSetTrusted(g.name, !isTrusted(g));
			await loadGroups();
		} catch (e) { toast(String(e), 'error'); }
	}

	// "+ New group" (M13 W5) — renders regardless of how many groups already exist, so a trusted
	// group (the on-ramp to M10 Private collections) is always reachable, not just from an existing
	// contact's group picker.
	let createGroupOpen = false;

	async function handleCreateGroup(e: CustomEvent<{ name: string; color: string; trusted: boolean }>) {
		const { name, color, trusted } = e.detail;
		try {
			await groupsCreate(name, color);
			if (trusted) await groupsSetTrusted(name, true);
			await loadGroups();
			createGroupOpen = false;
			toast(`Group "${name}" created`);
		} catch (e) { toast(String(e), 'error'); }
	}

	// Stale: last_fetched more than 7 days ago.
	function isStale(peer: CachedPeer): boolean {
		if (!peer.last_fetched) return false;
		return Date.now() - new Date(peer.last_fetched).getTime() > 7 * 24 * 60 * 60 * 1000;
	}

	// Which groups a contact belongs to (derived from groups[].pubkeys).
	function contactGroups(hb_id: string): string[] {
		return groups.filter(g => g.pubkeys.includes(hb_id)).map(g => g.name);
	}

	// Per-contact group-change select: hb_id → selected group name or '' for Ungrouped.
	let contactGroupEditing: Record<string, boolean> = {};

	async function handleMoveGroup(hb_id: string, newGroupName: string) {
		const groupNames = newGroupName ? [newGroupName] : [];
		try {
			await contactUpdateGroups(hb_id, groupNames);
			await loadGroups();
		} catch (e) { toast(String(e), 'error'); }
		contactGroupEditing = { ...contactGroupEditing, [hb_id]: false };
	}

	onMount(() => {
		loadGroups();
		loadPrivate();
		refreshOnline();
		onlinePollTimer = setInterval(refreshOnline, ONLINE_POLL_VISIBLE_MS);
		// Refresh all contacts in parallel on page load (task 4)
		$contacts.forEach(async (c) => {
			try {
				const updated = await refreshContact(c.npub);
				contacts.update(cs => cs.map(x => x.npub === c.npub ? { ...x, ...updated, local_tags: x.local_tags } : x));
			} catch { /* silent — relay may be unreachable */ }
		});
	});

	// Lookup state
	let input = '';
	let loading = false;
	let following = false;
	let result: CachedPeer | null = null;

	$: alreadyFollowed = $contacts.some((c) => c.npub === result?.npub);

	// ── §6 Discovery (moved from Browse — devtest 2026-06-25 #6) ─────────────────────────────────
	// Search public teasers by tag / content-type across the relays. Results are the opt-in public
	// teaser ONLY — each peer's listings stay 🔒 browse-key-locked (DISC3). Collapsed by default so it
	// doesn't add clutter; "find people" now lives entirely on Contacts (lookup-by-id + discovery).
	let discoverOpen = false;
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
		if (!canDiscover) { discoverError = 'Enter at least one tag or content type to search.'; return; }
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

	// Add-contact dialog (M13 W5 Slice 2): both the lookup card and a discovery hit open the same
	// petname + group picker before actually adding — `addContactTarget` is whichever npub is pending.
	let addContactOpen = false;
	let addContactDisplayName = '';
	let addContactTarget: string | null = null;

	function openAddContact(npub: string, displayName: string) {
		addContactTarget = npub;
		addContactDisplayName = displayName;
		addContactOpen = true;
	}

	function followHit(hit: PeerSearchHit) {
		// bare npub only: awareness, NOT a browse-key (INV-2) — the dialog's Skip path preserves that.
		openAddContact(hit.npub, hit.display_name);
	}

	async function completeFollow(npub: string, group: string | null, petname: string | undefined) {
		following = true;
		try {
			await follow(npub, group ?? undefined, petname);
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			await loadGroups();
			toast(`Added ${petname || addContactDisplayName || npub.slice(0, 12) + '…'}`, 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			following = false;
		}
	}

	async function handleAddContactSave(e: CustomEvent<{ petname: string; group: string | null }>) {
		if (!addContactTarget) return;
		const npub = addContactTarget;
		addContactOpen = false;
		addContactTarget = null;
		await completeFollow(npub, e.detail.group, e.detail.petname);
	}

	async function handleAddContactSkip() {
		if (!addContactTarget) return;
		const npub = addContactTarget;
		addContactOpen = false;
		addContactTarget = null;
		await completeFollow(npub, null, undefined);
	}

	async function handleLookup() {
		const id = input.trim();
		if (!id) return;
		if (id === $identity?.npub) {
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

	function handleFollow() {
		if (!result) return;
		openAddContact(result.npub, result.profile?.display_name ?? '');
	}

	// Contacts list state
	let expanded: string | null = null;
	let refreshing: string | null = null;
	let autoRefreshing: string | null = null;

	async function handleExpand(peer: CachedPeer) {
		const id = peer.npub;
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
				contacts.update(cs => cs.map(c => c.npub === id ? { ...c, ...updated, local_tags: c.local_tags } : c));
			} catch { /* silent — don't nag if relay is unreachable */ }
			finally { autoRefreshing = null; }
		}
	}

	async function handleRefresh(hb_id: string) {
		refreshing = hb_id;
		try {
			const updated = await refreshContact(hb_id);
			contacts.update((cs) => cs.map((c) => (c.npub === hb_id ? { ...updated, local_tags: c.local_tags } : c)));
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
			contacts.update((cs) => cs.filter((c) => c.npub !== hb_id));
			toast('Contact removed');
		} catch (e) {
			toast(String(e), 'error');
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

	function lastSeenLabel(peer: import('$lib/types.js').CachedPeer): string {
		const ts = peer.last_fetched;
		if (!ts) return 'never';
		return formatLastSeen(ts);
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
			contacts.update(cs => cs.map(c => c.npub === hb_id ? { ...c, local_tags: newTags } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	async function handleRemoveTag(hb_id: string, current_tags: string[], tag: string) {
		const newTags = current_tags.filter(t => t !== tag);
		try {
			await setContactTags(hb_id, newTags);
			contacts.update(cs => cs.map(c => c.npub === hb_id ? { ...c, local_tags: newTags } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	// Filter by tag
	let filterTag = '';
	$: filteredContacts = filterTag
		? $contacts.filter(c => c.local_tags?.includes(filterTag))
		: $contacts;

	$: allTags = [...new Set($contacts.flatMap(c => c.local_tags ?? []))].sort();
</script>

<div class="contacts-shell">
<div class="contacts-main">
<!-- TopBar -->
<div class="topbar">
	<div>
		<div class="topbar-title">Contacts</div>
		<div class="topbar-sub">
			{$contacts.length} contact{$contacts.length !== 1 ? 's' : ''} · {$contacts.filter(c => c.online).length} online
		</div>
	</div>
	{#if chip.show}
		<span class="online-chip" class:online-chip-muted={chip.unknown} title={whyHint ? `Hoarders online now — ${whyHint}` : 'Hoarders online now'}>{chip.label}</span>
		{#if whyHint}
			<span class="online-why" title={whyHint}>({whyHint})</span>
		{/if}
	{/if}
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
					placeholder="npub1… or share code (hbk1…)"
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
								letter={(result.profile?.display_name ?? result.npub)[0].toUpperCase()}
								size={52}
								hue={avatarHue((result.profile?.display_name ?? result.npub)[0])}
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
								<span class="mono">{result.npub.slice(0, 18)}…{result.npub.slice(-4)}</span>
							</div>
							<div class="profile-actions">
								<button
									class="btn-primary btn-sm"
									on:click={handleFollow}
									disabled={alreadyFollowed || following}
								>
									{alreadyFollowed ? 'Added' : following ? '…' : 'Add contact'}
								</button>
							</div>
						</div>

						{#if result.profile?.bio}
							<p class="peer-bio">{result.profile.bio}</p>
						{/if}

						<!-- §7 impersonation fingerprint — your at-a-glance trust check for a stranger you
						     just looked up (bound to the npub, not the display name). -->
						{#if result.fingerprint}
							<div class="fp-row">
								<span class="fp-swatch" style="background:{result.fingerprint.colorHex}" />
								<span class="fp-words hb-mono">{result.fingerprint.words.join(' ')} {result.fingerprint.colorHex}</span>
								<FeatureTooltip key="fingerprint" />
							</div>
						{/if}

						<!-- Content types + tags are the only rich fields a public teaser carries, so they
						     are what a lookup can actually show (§4/§5). -->
						{#if (result.profile?.content_types?.length ?? 0) > 0}
							<div class="badge-row-sm">
								{#each result.profile?.content_types ?? [] as ct (ct)}
									<span class="ct-badge">{ct}</span>
								{/each}
							</div>
						{/if}
						{#if (result.profile?.tags?.length ?? 0) > 0}
							<div class="peer-tags">
								{#each result.profile?.tags ?? [] as tag (tag)}
									<span class="peer-tag">{tag}</span>
								{/each}
							</div>
						{/if}
					</div>
				</div>
			</div>
		{/if}
	</div>

	<!-- §6 Discover hoarders (moved from Browse — devtest 2026-06-25 #6). Collapsible so it doesn't
	     clutter Contacts; results are the opt-in public teaser only (listings stay 🔒 locked). -->
	<div class="discover-section">
		<button class="discover-toggle" on:click={() => (discoverOpen = !discoverOpen)} aria-expanded={discoverOpen}>
			<span class="discover-toggle-label">{@html icons.search} Discover hoarders</span>
			<span class="discover-chevron" class:open={discoverOpen}>{@html icons.chevronDown}</span>
		</button>
		{#if discoverOpen}
			<div class="discover-body">
				<div class="discover-sub">Search public profiles by tag &amp; content type. Only what people chose to announce is searchable — everyone's listings stay encrypted.</div>
				<div class="ct-row">
					{#each DISCOVER_CONTENT_TYPES as ct (ct.value)}
						<button type="button" class="ct-chip" class:ct-on={discoverTypes.includes(ct.value)}
							on:click={() => (discoverTypes = toggleContentType(discoverTypes, ct.value))}>{ct.label}</button>
					{/each}
				</div>
				<form class="disc-tag-row" on:submit|preventDefault={runDiscover}>
					<input class="disc-tag-input" placeholder="tags (e.g. anime, vhs)" bind:value={discoverTags} />
					<button class="btn-primary btn-sm" type="submit" disabled={!canDiscover || discovering}>
						{discovering ? 'Searching…' : 'Search'}
					</button>
				</form>
				{#if discoverError}<div class="discover-error">{discoverError}</div>{/if}
				{#if discovering}
					<div class="discover-empty">Searching the relays…</div>
				{:else if discovered && discoverResults.length === 0}
					<div class="discover-empty">No hoarders matched those filters.</div>
				{:else if discovered}
					<div class="discover-results">
						{#each discoverResults as hit (hit.npub)}
							{@const letter = (hit.display_name?.[0] ?? hit.npub[0]).toUpperCase()}
							{@const isContact = followedNpubs.has(hit.npub)}
							<div class="hit-card">
								<div class="hit-top">
									<Avatar {letter} size={30} hue={avatarHue(letter)} />
									<div class="hit-id">
										<span class="hit-name">{hit.display_name || hit.npub.slice(0, 12) + '…'}</span>
										{#if !isContact}<span class="hit-stranger" title="Verify the fingerprint before trusting a stranger">unverified — not in your contacts</span>{/if}
									</div>
									{#if isContact}
										<span class="hit-following">Added</span>
									{:else}
										<button class="hit-follow" on:click={() => followHit(hit)}>Add contact</button>
									{/if}
								</div>
								{#if hit.bio}<div class="hit-bio">{hit.bio}</div>{/if}
								{#if hit.fingerprint}
									<div class="hit-fp" title="Identity fingerprint — check it before trusting a stranger">
										<span class="hit-fp-swatch" style="background:{hit.fingerprint.colorHex}"></span>
										{renderFingerprint(hit.fingerprint)}
									</div>
								{/if}
								{#if hit.content_types.length > 0 || hit.tags.length > 0}
									<div class="hit-tags">
										{#each hit.content_types as ct}<span class="hit-tag hit-tag-ct">{ct}</span>{/each}
										{#each hit.tags.slice(0, 6) as t}<span class="hit-tag">#{t}</span>{/each}
									</div>
								{/if}
								<div class="hit-locked">🔒 Listings locked<FeatureTooltip key="listings-locked" /></div>
							</div>
						{/each}
					</div>
				{:else}
					<div class="discover-empty">Pick a content type or enter a tag, then Search.</div>
				{/if}
			</div>
		{/if}
	</div>

	<!-- Divider + tag filter -->
	<div class="section-divider">
		<div class="divider-line" />
		<span class="divider-label">Contacts ({$contacts.length})</span>
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

	<!-- Trusted groups (M10): mark a group trusted to seal Private collections to its members. Always
	     rendered — even with zero groups — so "+ New group" (the on-ramp to a trusted group) is never
	     an unreachable dead path. -->
	<div class="trusted-groups">
		<div class="trusted-label">Trusted groups <span class="trusted-hint">— receive your Private collections</span></div>
		<div class="trusted-chips">
			{#each groups as g (g.name)}
				<label class="trusted-chip" class:is-trusted={isTrusted(g)}>
					<input type="checkbox" checked={isTrusted(g)} on:change={() => toggleTrusted(g)} />
					{g.name}
				</label>
			{/each}
			<button type="button" class="trusted-chip trusted-chip-add" on:click={() => (createGroupOpen = true)}>+ New group</button>
		</div>
	</div>

	<!-- Contacts list -->
	{#if $contacts.length === 0}
		<div class="empty">No contacts yet. Look up a peer above and add them.</div>
	{:else if filteredContacts.length === 0}
		<div class="empty">No contacts with tag "{filterTag}".</div>
	{:else}
		<div class="contact-list">
			{#each filteredContacts as peer}
				{@const name = contactDisplayName(peer)}
				{@const initial = name[0]?.toUpperCase() ?? '?'}
				{@const hue = avatarHue(initial)}
				{@const peerGroups = contactGroups(peer.npub)}
				{@const stale = isStale(peer) && !peer.online}
				<div class="contact-block">
					<div class="contact-card">
						<Avatar letter={initial} size={34} {hue} />
						<div class="contact-info">
							<div class="name-row">
								<span class="peer-name">{name}</span>
								{#if peer.online}
									<span class="pill pill-online"><span class="pill-dot" /></span>
								{:else if stale}
									<span class="pill pill-stale" title="Last fetched {new Date(peer.last_fetched).toLocaleDateString()}">Stale</span>
								{:else}
									<span class="pill pill-offline">Offline</span>
								{/if}
								<span class="last-seen">seen {lastSeenLabel(peer)}</span>
								<div style="flex:1" />
								<button
									class="btn-ghost btn-xs btn-icon"
									on:click={() => handleRefresh(peer.npub)}
									disabled={refreshing === peer.npub}
									title="Refresh"
								>
									<span>{@html icons.refresh}</span>
								</button>
								<ConfirmButton
									label="Remove contact"
									confirmText="Remove this contact?"
									on:confirm={() => handleUnfollow(peer.npub)}
								/>
								<button class="btn-default btn-xs" on:click={() => handleExpand(peer)}>
									{expanded === peer.npub ? 'Hide' : 'Browse'}
								</button>
							</div>
							<div class="contact-sub-row">
								<div class="mono">{shortId(peer.npub)}</div>
								{#if peer.profile?.est_size}<span class="sub-dot">·</span><span class="sub-meta">~{peer.profile.est_size}</span>{/if}
								{#if peer.collections.length > 0}<span class="sub-dot">·</span><span class="sub-meta">{peer.collections.length} collection{peer.collections.length !== 1 ? 's' : ''}</span>{/if}
							</div>

							<!-- Groups -->
							{#if peerGroups.length > 0 || contactGroupEditing[peer.npub]}
								<div class="group-row">
									{#each peerGroups as gname}
										<span class="group-pill">{gname}</span>
									{/each}
									{#if contactGroupEditing[peer.npub]}
										<select
											class="group-select group-select-inline"
											on:change={(e) => handleMoveGroup(peer.npub, e.currentTarget.value)}
										>
											<option value="">Ungrouped</option>
											{#each groups as g (g.name)}
												<option value={g.name} selected={peerGroups.includes(g.name)}>{g.name}</option>
											{/each}
										</select>
										<button class="tag-x" on:click={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: false }; }}>×</button>
									{:else if groups.length > 0}
										<button class="tag-add-btn" on:click={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: true }; }}>
											{peerGroups.length > 0 ? '✎' : '+ group'}
										</button>
									{/if}
								</div>
							{:else if groups.length > 0}
								<div class="group-row">
									<button class="tag-add-btn" on:click={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: true }; }}>+ group</button>
								</div>
							{/if}

							<!-- Local tags -->
							<div class="tag-row">
								{#each (peer.local_tags ?? []) as tag}
									<span class="local-tag">
										{tag}
										<button class="tag-x" on:click={() => handleRemoveTag(peer.npub, peer.local_tags ?? [], tag)}>×</button>
									</span>
								{/each}
								{#if editingTagsFor === peer.npub}
									<input
										class="tag-input"
										type="text"
										placeholder="tag…"
										bind:value={tagInput}
										on:keydown={(e) => {
											if (e.key === 'Enter' || e.key === ',') { e.preventDefault(); handleAddTag(peer.npub, peer.local_tags ?? []); }
											if (e.key === 'Escape') { editingTagsFor = null; tagInput = ''; }
										}}
										on:blur={() => { editingTagsFor = null; tagInput = ''; }}
									/>
								{:else}
									<button class="tag-add-btn" on:click={() => { editingTagsFor = peer.npub; tagInput = ''; }}>+ tag</button>
								{/if}
							</div>
						</div>
					</div>

					{#if expanded === peer.npub}
						<div class="collections-indent">
							{#if peer.profile?.bio}
								<p class="contact-bio">{peer.profile.bio}</p>
							{/if}
							{#if autoRefreshing === peer.npub}
								<p class="no-coll">Checking for collections…</p>
							{:else if peer.collections.length === 0}
								<p class="no-coll">No collections published.</p>
							{:else}
								{#each peer.collections as col}
									<CollectionPanel collection={col} />
								{/each}
							{/if}

							<!-- Private collections this peer has sealed to me (M10). Absent (not
							     "locked") for a non-trusted viewer — nothing to show, no hint. -->
							{#if (privateByAuthor[peer.npub] ?? []).length > 0}
								<div class="private-section">
									<div class="section-label">Private collections <span class="private-badge">trusted</span></div>
									{#each privateByAuthor[peer.npub] as col}
										<CollectionPanel collection={col} />
									{/each}
									<p class="not-drm-note">{NOT_DRM_NOTE}</p>
								</div>
							{/if}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>
</div>
</div>

<CreateGroupDialog bind:open={createGroupOpen} on:create={handleCreateGroup} on:cancel={() => (createGroupOpen = false)} />
<AddContactDialog
	bind:open={addContactOpen}
	displayName={addContactDisplayName}
	{groups}
	on:save={handleAddContactSave}
	on:skip={handleAddContactSkip}
	on:newGroup={() => (createGroupOpen = true)}
	on:cancel={() => { addContactOpen = false; addContactTarget = null; }}
/>

<style>
	.contacts-shell {
		display: flex;
		flex: 1;
		overflow: hidden;
		min-width: 0;
	}
	.contacts-main {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

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
	.online-chip { font-size: 12px; font-weight: 600; color: var(--fg-dim); white-space: nowrap; }
	.online-chip-muted { opacity: 0.55; }
	.online-why { font-size: 10.5px; color: var(--fg-dim); opacity: 0.7; white-space: nowrap; }

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

	/* §7 fingerprint row on the lookup card */
	.fp-row { display: flex; align-items: center; gap: 7px; margin-top: 2px; }
	.fp-swatch {
		width: 14px; height: 14px; border-radius: 4px;
		border: 1px solid var(--border-strong); flex-shrink: 0;
	}
	.fp-words { font-size: 11.5px; color: var(--fg-muted); }

	/* Content-type badges + profile tags — the rich public fields a teaser carries */
	.badge-row-sm { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
	.ct-badge {
		font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
		background: var(--bg-elev3); color: var(--fg-muted);
		border: 1px solid var(--border);
	}
	.peer-tags { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
	.peer-tag {
		font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
		color: var(--accent);
		background: color-mix(in oklch, var(--accent) 12%, transparent);
	}

	.section-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
	}

	/* M10 — trusted-groups strip + private-collection display */
	.trusted-groups { padding: 8px 0; display: flex; flex-direction: column; gap: 6px; }
	.trusted-label { font-size: 11.5px; font-weight: 600; color: var(--fg-muted); }
	.trusted-hint { font-weight: 400; color: var(--fg-dim); }
	.trusted-chips { display: flex; flex-wrap: wrap; gap: 6px; }
	.trusted-chip {
		display: inline-flex; align-items: center; gap: 5px;
		padding: 3px 9px; border: 1px solid var(--border); border-radius: 999px;
		font-size: 12px; cursor: pointer; color: var(--fg-muted);
	}
	.trusted-chip.is-trusted {
		border-color: var(--accent);
		color: var(--accent);
		background: color-mix(in oklch, var(--accent) 12%, transparent);
	}
	.trusted-chip-add {
		background: transparent;
		border-style: dashed;
		font-family: var(--font-ui);
		color: var(--fg-dim);
	}
	.trusted-chip-add:hover { border-color: var(--accent); color: var(--accent); }
	.private-section { margin-top: 10px; display: flex; flex-direction: column; gap: 8px; }
	.private-badge {
		font-size: 9.5px; padding: 1px 6px; border-radius: 999px; letter-spacing: 0.5px;
		background: color-mix(in oklch, var(--accent) 16%, transparent); color: var(--accent);
	}
	.not-drm-note { margin: 2px 0 0; font-size: 11px; line-height: 1.4; color: var(--fg-dim); }

	/* Divider */
	.section-divider {
		display: flex;
		align-items: center;
		gap: 8px;
		margin: 4px 0 20px;
	}

	.icon-btn {
		background: transparent; border: none; cursor: pointer;
		color: var(--fg-muted); display: flex; padding: 2px; flex-shrink: 0;
	}
	.icon-btn:disabled { opacity: 0.5; cursor: not-allowed; }

	.divider-line { flex: 1; height: 1px; background: var(--divider); }

	.divider-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
		white-space: nowrap;
	}

	/* Contacts list */
	.empty { color: var(--fg-dim); font-size: 13px; padding: 16px 0; }

	.contact-list { display: flex; flex-direction: column; gap: 12px; }

	.contact-block { display: flex; flex-direction: column; gap: 8px; }

	.contact-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 10px 12px;
		display: flex;
		gap: 10px;
		align-items: flex-start;
	}

	.contact-info { flex: 1; min-width: 0; }

	.last-seen { font-size: 10.5px; color: var(--fg-dim); }

	.contact-sub-row {
		display: flex; align-items: center; gap: 5px;
		margin-top: 2px; font-size: 11px; color: var(--fg-muted);
	}
	.sub-dot { color: var(--fg-dim); }
	.sub-meta { color: var(--fg-muted); font-feature-settings: 'tnum'; }

	/* Modal */
	.modal-overlay {
		position: fixed; inset: 0;
		background: oklch(0 0 0 / 0.6);
		display: flex; align-items: center; justify-content: center;
		z-index: 9000;
	}
	.modal {
		background: var(--bg-elev2);
		border: 1px solid var(--border-strong);
		border-radius: 12px;
		padding: 22px;
		max-width: 400px;
		width: calc(100vw - 48px);
		box-shadow: 0 20px 60px oklch(0 0 0 / 0.5);
	}
	.modal-title {
		font-size: 15px; font-weight: 600; color: var(--fg);
		margin-bottom: 10px; display: flex; gap: 8px; align-items: center;
	}
	.modal-body { font-size: 13px; color: var(--fg-muted); line-height: 1.55; margin: 0 0 18px; }
	.modal-actions { display: flex; justify-content: flex-end; gap: 8px; }

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

	.contact-bio { font-size: 12.5px; color: var(--fg-muted); line-height: 1.55; margin: 0 0 6px; }

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
	.pill-stale {
		color: oklch(0.75 0.12 60);
		background: oklch(0.22 0.06 60 / 0.4);
		border: 1px solid oklch(0.50 0.10 60 / 0.3);
	}

	/* Group row on contact cards */
	.group-row { display: flex; flex-wrap: wrap; gap: 4px; margin: 3px 0 2px; align-items: center; min-height: 20px; }
	.group-pill {
		display: inline-flex; align-items: center;
		padding: 1px 8px; border-radius: 4px; font-size: 11px; font-weight: 500;
		background: color-mix(in oklch, var(--accent) 10%, transparent);
		border: 1px solid color-mix(in oklch, var(--accent) 22%, transparent);
		color: var(--accent);
	}
	.group-select {
		height: 26px; padding: 0 6px; border-radius: 5px; font-size: 11.5px;
		background: var(--bg-input); border: 1px solid var(--border); color: var(--fg);
		font-family: var(--font-ui); cursor: pointer;
	}
	.group-select-inline { height: 22px; font-size: 11px; }

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
	.btn-xs { padding: 3px 8px; font-size: 11px; height: 24px; }
	.btn-icon { gap: 4px; }

	/* ── §6 Discover hoarders (moved from Browse — devtest 2026-06-25 #6) ──────────────────────── */
	.discover-section {
		margin-bottom: 18px;
		border: 1px solid var(--border);
		border-radius: 9px;
		background: var(--bg-elev1);
		overflow: hidden;
	}
	.discover-toggle {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 10px 14px;
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg);
		font-family: var(--font-ui);
	}
	.discover-toggle:hover { background: var(--bg-elev2); }
	.discover-toggle-label { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; font-weight: 600; }
	.discover-chevron { display: flex; color: var(--fg-muted); transition: transform 0.15s; }
	.discover-chevron.open { transform: rotate(180deg); }
	.discover-body { padding: 4px 14px 14px; border-top: 1px solid var(--divider); display: flex; flex-direction: column; gap: 10px; }
	.discover-sub { font-size: 11.5px; color: var(--fg-dim); margin-top: 8px; }
	.ct-row { display: flex; flex-wrap: wrap; gap: 6px; }
	.ct-chip {
		font-size: 11.5px; padding: 4px 11px; border-radius: 999px;
		background: var(--bg-elev2); color: var(--fg-muted);
		border: 1px solid var(--border); cursor: pointer; font-family: var(--font-ui);
		transition: background 0.1s, color 0.1s, border-color 0.1s;
	}
	.ct-chip:hover { background: var(--bg-elev3); }
	.ct-on { background: var(--accent-soft); color: var(--accent); border-color: color-mix(in oklch, var(--accent) 35%, transparent); font-weight: 600; }
	.disc-tag-row { display: flex; gap: 8px; }
	.disc-tag-input {
		flex: 1; background: var(--bg-elev2); border: 1px solid var(--border); border-radius: 7px;
		padding: 7px 10px; font-size: 12.5px; color: var(--fg); font-family: var(--font-ui); outline: none;
	}
	.disc-tag-input::placeholder { color: var(--fg-dim); }
	.disc-tag-input:focus { border-color: var(--accent); }
	.discover-error { font-size: 11.5px; color: oklch(0.75 0.15 25); }
	.discover-results { display: grid; grid-template-columns: repeat(auto-fill, minmax(232px, 1fr)); gap: 12px; }
	.discover-empty { text-align: center; color: var(--fg-dim); font-size: 12.5px; padding: 18px 0; }
	.hit-card {
		display: flex; flex-direction: column; gap: 7px; padding: 13px;
		background: var(--bg-elev2); border: 1px solid var(--border); border-radius: 9px;
	}
	.hit-top { display: flex; align-items: center; gap: 9px; }
	.hit-id { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 1px; }
	.hit-name { font-size: 13px; font-weight: 600; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.hit-stranger { font-size: 9.5px; color: oklch(0.72 0.13 70); }
	.hit-follow {
		padding: 4px 12px; border-radius: 6px; background: var(--accent); color: var(--accent-text);
		border: none; font-size: 11.5px; font-weight: 600; cursor: pointer; font-family: var(--font-ui); flex-shrink: 0;
	}
	.hit-following { font-size: 11px; color: var(--fg-dim); flex-shrink: 0; }
	.hit-bio { font-size: 11.5px; color: var(--fg-muted); overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
	.hit-fp { display: flex; align-items: center; gap: 6px; font-size: 10px; color: var(--fg-dim); font-family: var(--font-mono); }
	.hit-fp-swatch { width: 10px; height: 10px; border-radius: 3px; flex-shrink: 0; }
	.hit-tags { display: flex; flex-wrap: wrap; gap: 4px; }
	.hit-tag { font-size: 9.5px; padding: 1px 5px; border-radius: 999px; background: var(--bg-elev3); color: var(--fg-muted); border: 1px solid var(--border); }
	.hit-tag-ct { background: var(--accent-soft); color: var(--accent); border-color: color-mix(in oklch, var(--accent) 30%, transparent); }
	.hit-locked { display: inline-flex; align-items: center; font-size: 11px; color: var(--fg-dim); margin-top: 2px; }
</style>
