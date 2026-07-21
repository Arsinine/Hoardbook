<script lang="ts">
	import { follow, refreshContact, unfollowContact, setContactTags, groupsGet, groupsCreate, groupsDelete, contactUpdateGroups, groupsSetTrusted, browsePrivateCollections, onlineCount, relayStatus, getContacts, type OnlineCount, type RelayHealth } from '$lib/api.js';
	import { contacts, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import CollectionPanel from '$lib/components/CollectionPanel.svelte';
	import OverflowMenu from '$lib/components/OverflowMenu.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';
	import CreateGroupDialog from '$lib/components/CreateGroupDialog.svelte';
	import AddContactDialog from '$lib/components/AddContactDialog.svelte';
	import AddContactPanel from '$lib/components/AddContactPanel.svelte';
	import AZRail from '$lib/components/AZRail.svelte';
	import type { CachedPeer, Collection, Group } from '$lib/types.js';
	import { contactDisplayName } from '$lib/contact-display.js';
	import { NOT_DRM_NOTE, isTrusted } from '$lib/private-collections-view.js';
	import { peerAccessBadge, summarizeCollectionsSize } from '$lib/browse-view.js';
	import { onlineChipView } from '$lib/online-chip.js';
	import { relayWhyHint } from '$lib/relay-health.js';
	import { ONLINE_POLL_VISIBLE_MS } from '$lib/poll-lifecycle.js';
	import { ALPHABET, groupByLetter, groupByGroups, onlineBucket, matchesQuery, presentSectionKeys } from '$lib/contacts-view.js';
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';

	// ── "🟢 N hoarders online" chip (M9) — relay-derived, no telemetry. **Always shown** (the Settings
	//    hide-toggle was removed); lives here on Contacts. Best-effort + cached on the backend; polled on
	//    a slow tick (L4-budgeted); shows "–" while the count is unknown (m4).
	let onlineData: OnlineCount | null = $state(null);
	let relayHealth: RelayHealth[] = $state([]);
	let chip = $derived(onlineChipView(onlineData, true));
	// M12 W1 Decision D: when the chip can't show a number, say *why* (which relays are unreachable).
	let whyHint = $derived(chip.unknown ? relayWhyHint(relayHealth) : '');
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
	let groups: Group[] = $state([]);

	async function loadGroups() {
		try { groups = await groupsGet(); } catch { /* non-fatal */ }
	}

	// M10: Private collections trusted peers have sealed to me, keyed by author npub. A non-trusted
	// viewer simply has no entry — there is no locked-teaser hint.
	let privateByAuthor: Record<string, Collection[]> = $state({});

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

	// Delete a group (devtest #2) — the backend (groups_delete) moves members to Ungrouped; a
	// trusted group additionally stops sealing them Private collections on the next publish, so both
	// the groups strip and the Private-collections recipient view need a refresh.
	async function handleDeleteGroup(g: Group) {
		try {
			await groupsDelete(g.name);
			await loadGroups();
			await loadPrivate();
			toast(`Group "${g.name}" deleted`);
		} catch (e) { toast(String(e), 'error'); }
	}

	// "+ New group" (M13 W5) — renders regardless of how many groups already exist, so a trusted
	// group (the on-ramp to M10 Private collections) is always reachable, not just from an existing
	// contact's group picker.
	let createGroupOpen = $state(false);

	async function handleCreateGroup(detail: { name: string; color: string; trusted: boolean }) {
		const { name, color, trusted } = detail;
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
	let contactGroupEditing: Record<string, boolean> = $state({});

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

	// Add-contact dialog (M13 W5 Slice 2): both the lookup card and a discovery hit open the same
	// petname + group picker before actually adding — `addContactTarget` is whichever npub is pending.
	let addContactOpen = $state(false);
	let addContactDisplayName = $state('');
	let addContactTarget: string | null = $state(null);
	// The share code `follow` must re-resolve (full `hbk1…` for a lookup, npub for a discovery hit).
	// Kept alongside the npub target so the browse-key isn't dropped in the funnel (devtest #3).
	let addContactCode: string | null = $state(null);

	function openAddContact(code: string, npub: string, displayName: string) {
		addContactCode = code;
		addContactTarget = npub;
		addContactDisplayName = displayName;
		addContactOpen = true;
	}

	async function completeFollow(code: string, npub: string, group: string | null, petname: string | undefined) {
		try {
			await follow(code, group ?? undefined, petname);
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			await loadGroups();
			toast(`Added ${petname || addContactDisplayName || npub.slice(0, 12) + '…'}`, 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleAddContactSave(detail: { petname: string; group: string | null }) {
		if (!addContactTarget || addContactCode === null) return;
		const npub = addContactTarget;
		const code = addContactCode;
		addContactOpen = false;
		addContactTarget = null;
		addContactCode = null;
		await completeFollow(code, npub, detail.group, detail.petname);
	}

	async function handleAddContactSkip() {
		if (!addContactTarget || addContactCode === null) return;
		const npub = addContactTarget;
		const code = addContactCode;
		addContactOpen = false;
		addContactTarget = null;
		addContactCode = null;
		await completeFollow(code, npub, null, undefined);
	}

	// "+ Add contact" (devtest #17/#18 redesign) — the lookup-by-ID + §6 Discover surfaces now live
	// behind a single centered modal panel instead of cluttering the page. Both of its add entry
	// points call back through `openAddContact`, same funnel as before.
	let addContactPanelOpen = $state(false);

	// Contacts list state (M15 W5: chevron toggles the light-detail area; browsing a peer's
	// collections moved to the Browse tab via the `/browse?peer=` deep-link — no inline expansion).
	let detailExpanded: string | null = $state(null);
	let refreshing: string | null = $state(null);
	let menuOpenFor: string | null = $state(null);
	let menuAnchor: HTMLElement | undefined = $state();

	function toggleDetail(npub: string) {
		detailExpanded = detailExpanded === npub ? null : npub;
	}

	function openRowMenu(npub: string, anchor: HTMLElement) {
		menuAnchor = anchor;
		menuOpenFor = npub;
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
	let editingTagsFor: string | null = $state(null);
	let tagInput = $state('');

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

	// Filter by tag — demoted to a collapsible row (default collapsed), applied in both views.
	let filterTag = $state('');
	let tagFilterOpen = $state(false);
	let allTags = $derived([...new Set($contacts.flatMap(c => c.local_tags ?? []))].sort());

	// ── Phonebook redesign (devtest #17/#18): sticky free-text search + Name|Groups view toggle +
	//    a pinned "Online now" bucket (additive — an online peer also still appears in its section). ──
	let searchQuery = $state('');
	let view: 'name' | 'groups' = $state('name');

	let visible = $derived(
		$contacts.filter(c => matchesQuery(c, searchQuery)).filter(c => !filterTag || (c.local_tags ?? []).includes(filterTag))
	);
	// #1: an online peer moves OUT of its A-Z section INTO the pinned "Online now" bucket (never both),
	//     and moves back when it goes offline. #8: the Groups view is for organizing, so it has no
	//     Online-now bucket and every group lists all its members (online included).
	let online = $derived(view === 'name' ? onlineBucket(visible) : []);
	let sections = $derived(
		view === 'name' ? groupByLetter(visible.filter((c) => !c.online)) : groupByGroups(visible, groups)
	);
	let railTargets = $derived(
		ALPHABET.map(l => ({ label: l, anchorId: l === '#' ? 'sec-hash' : `sec-${l}`, enabled: presentSectionKeys(sections).has(l) }))
	);
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

<!-- Sticky sub-header: free-text search, Name|Groups view toggle, "+ Add contact" -->
<div class="subheader">
	<div class="subheader-search">
		<span class="search-icon">{@html icons.search}</span>
		<input type="text" placeholder="Search name, bio, tags, collections…" bind:value={searchQuery} />
	</div>
	<div class="view-toggle" role="group" aria-label="View">
		<button type="button" aria-pressed={view === 'name'} onclick={() => (view = 'name')}>Name</button>
		<button type="button" aria-pressed={view === 'groups'} onclick={() => (view = 'groups')}>Groups</button>
	</div>
	<button type="button" class="btn-primary btn-sm" onclick={() => (addContactPanelOpen = true)}>+ Add contact</button>
</div>

{#if allTags.length > 0}
	<div class="tagfilter-row">
		<button type="button" class="tagfilter-toggle" onclick={() => (tagFilterOpen = !tagFilterOpen)} aria-expanded={tagFilterOpen}>
			Filter by tag <span class="tagfilter-chevron" class:open={tagFilterOpen}>{@html icons.chevronDown}</span>
		</button>
		{#if tagFilterOpen}
			<div class="tag-filter-row">
				<button class="filter-tag" class:filter-tag-active={!filterTag} onclick={() => filterTag = ''}>All</button>
				{#each allTags as tag}
					<button class="filter-tag" class:filter-tag-active={filterTag === tag} onclick={() => filterTag = filterTag === tag ? '' : tag}>{tag}</button>
				{/each}
			</div>
		{/if}
	</div>
{/if}

{#snippet contactRow(peer: CachedPeer)}
	{@const name = contactDisplayName(peer)}
	{@const initial = name[0]?.toUpperCase() ?? '?'}
	{@const hue = avatarHue(initial)}
	{@const peerGroups = contactGroups(peer.npub)}
	{@const stale = isStale(peer) && !peer.online}
	{@const badge = peerAccessBadge(peer)}
	{@const sizeSummary = !badge.locked ? summarizeCollectionsSize(peer.collections) : null}
	{@const isOpen = detailExpanded === peer.npub}
	<div class="contact-block">
		<!-- devtest v0.12.1 #4: double-click a contact to open the conversation in Chat. The chevron,
		     Browse, and ⋯ controls keep their own single-click actions. -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<!-- Ignore double-clicks that land on an inner control (chevron, Browse, ⋯ menu) so they keep
		     their own single-click action instead of also navigating to Chat (codex review). -->
		<div class="contact-card" ondblclick={(e) => { if ((e.target as HTMLElement).closest('button, a')) return; goto('/chat?peer=' + peer.npub); }} title="Double-click to message in Chat">
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<button class="chevron-btn" onclick={() => toggleDetail(peer.npub)} aria-expanded={isOpen} aria-label="Toggle details">
				<span class="chevron" class:chevron-open={isOpen}>{@html icons.chevronDown}</span>
			</button>
			<div class="avatar-wrap">
				<Avatar letter={initial} size={34} {hue} picture={peer.profile?.picture} />
				{#if badge.locked}
					<span class="lock-overlay" title={badge.hint}>🔒</span>
				{/if}
			</div>
			<div class="contact-info">
				<div class="name-row">
					<span class="peer-name">{name}</span>
					{#if peer.online}
						<span class="pill pill-online"><span class="pill-dot"></span></span>
					{:else if stale}
						<span class="pill pill-stale" title="Last fetched {new Date(peer.last_fetched).toLocaleDateString()}">Stale</span>
					{:else}
						<span class="pill pill-offline">Offline</span>
					{/if}
					<span class="last-seen">seen {lastSeenLabel(peer)}</span>
					<div style="flex:1"></div>
					<a class="btn-default btn-xs" href="/browse?peer={peer.npub}">Browse</a>
					<button
						class="row-menu-btn"
						aria-label="Contact actions"
						aria-haspopup="true"
						aria-expanded={menuOpenFor === peer.npub}
						onclick={(e) => openRowMenu(peer.npub, e.currentTarget)}
					>⋯</button>
				</div>
				<div class="contact-sub-row">
					<div class="mono">{shortId(peer.npub)}</div>
					{#if peer.collections.length > 0}<span class="sub-dot">·</span><span class="sub-meta">{peer.collections.length} collection{peer.collections.length !== 1 ? 's' : ''}</span>{/if}
					{#if !badge.locked}
						{#if sizeSummary}<span class="sub-dot">·</span><span class="sub-meta">{sizeSummary}</span>
						{:else if peer.profile?.est_size}<span class="sub-dot">·</span><span class="sub-meta">~{peer.profile.est_size}</span>{/if}
					{/if}
				</div>
				{#if badge.locked}
					<div class="access-hint">{badge.hint}</div>
				{/if}
			</div>
		</div>

		{#if isOpen}
			<div class="contact-detail">
				{#if peer.profile?.bio}
					<p class="card-bio">{peer.profile.bio}</p>
				{/if}
				{#if (peer.profile?.content_types?.length ?? 0) > 0}
					<div class="badge-row-sm">
						{#each peer.profile?.content_types ?? [] as ct (ct)}
							<span class="ct-badge">{ct}</span>
						{/each}
					</div>
				{/if}
				{#if (peer.profile?.tags?.length ?? 0) > 0}
					<div class="peer-tags">
						{#each peer.profile?.tags ?? [] as tag (tag)}
							<span class="peer-tag">{tag}</span>
						{/each}
					</div>
				{/if}

				<!-- Groups -->
				{#if peerGroups.length > 0 || contactGroupEditing[peer.npub]}
					<div class="group-row">
						{#each peerGroups as gname}
							<span class="group-pill">{gname}</span>
						{/each}
						{#if contactGroupEditing[peer.npub]}
							<select
								class="group-select group-select-inline"
								onchange={(e) => handleMoveGroup(peer.npub, e.currentTarget.value)}
							>
								<option value="">Ungrouped</option>
								{#each groups as g (g.name)}
									<option value={g.name} selected={peerGroups.includes(g.name)}>{g.name}</option>
								{/each}
							</select>
							<button class="tag-x" onclick={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: false }; }}>×</button>
						{:else if groups.length > 0}
							<button class="tag-add-btn" onclick={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: true }; }}>
								{peerGroups.length > 0 ? '✎' : '+ group'}
							</button>
						{/if}
					</div>
				{:else if groups.length > 0}
					<div class="group-row">
						<button class="tag-add-btn" onclick={() => { contactGroupEditing = { ...contactGroupEditing, [peer.npub]: true }; }}>+ group</button>
					</div>
				{/if}

				<!-- Local tags -->
				<div class="tag-row">
					{#each (peer.local_tags ?? []) as tag}
						<span class="local-tag">
							{tag}
							<button class="tag-x" onclick={() => handleRemoveTag(peer.npub, peer.local_tags ?? [], tag)}>×</button>
						</span>
					{/each}
					{#if editingTagsFor === peer.npub}
						<input
							class="tag-input"
							type="text"
							placeholder="tag…"
							bind:value={tagInput}
							onkeydown={(e) => {
								if (e.key === 'Enter' || e.key === ',') { e.preventDefault(); handleAddTag(peer.npub, peer.local_tags ?? []); }
								if (e.key === 'Escape') { editingTagsFor = null; tagInput = ''; }
							}}
							onblur={() => { editingTagsFor = null; tagInput = ''; }}
						/>
					{:else}
						<button class="tag-add-btn" onclick={() => { editingTagsFor = peer.npub; tagInput = ''; }}>+ tag</button>
					{/if}
				</div>

				<!-- Private collections sealed to me (M10) — not served by the Browse deep-link, so
				     they stay here in the detail area. Absent (not "locked") for a non-trusted viewer. -->
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

	<!-- M15 W5: per-row overflow menu (only one open at a time via menuOpenFor). -->
	<OverflowMenu open={menuOpenFor === peer.npub} anchor={menuAnchor} onclose={() => (menuOpenFor = null)}>
		<button class="menu-item" onclick={() => { handleRefresh(peer.npub); menuOpenFor = null; }} disabled={refreshing === peer.npub}>
			{refreshing === peer.npub ? 'Refreshing…' : 'Refresh'}
		</button>
		<button class="menu-item" onclick={() => { detailExpanded = peer.npub; contactGroupEditing = { ...contactGroupEditing, [peer.npub]: true }; menuOpenFor = null; }}>Edit groups…</button>
		<button class="menu-item" onclick={() => { detailExpanded = peer.npub; editingTagsFor = peer.npub; tagInput = ''; menuOpenFor = null; }}>Edit tags…</button>
		<div class="menu-item menu-item-confirm">
			<ConfirmButton label="Remove contact" confirmText="Remove this contact?" onconfirm={() => { handleUnfollow(peer.npub); menuOpenFor = null; }} />
		</div>
	</OverflowMenu>
{/snippet}

<div class="phonebook">
	<div class="phonebook-scroll">
		{#if $contacts.length === 0}
			<div class="empty">No contacts yet. Use “+ Add contact” to find someone by ID or discover hoarders.</div>
		{:else}
			{#if view === 'groups'}
				<!-- Trusted groups (M10): mark a group trusted to seal Private collections to its members.
				     Groups-view-only — this strip is the group-management surface, so it belongs beside the
				     view it organizes; the Name view has no use for it. -->
				<div class="trusted-groups">
					<div class="trusted-label">Trusted groups <span class="trusted-hint">— receive your Private collections</span></div>
					<div class="trusted-chips">
						{#each groups as g (g.name)}
							<span class="trusted-chip-wrap">
								<label class="trusted-chip" class:is-trusted={isTrusted(g)}>
									<input type="checkbox" checked={isTrusted(g)} onchange={() => toggleTrusted(g)} />
									{g.name}
								</label>
								<ConfirmButton
									label="×"
									confirmText={isTrusted(g)
										? `Delete "${g.name}"? Its members stop receiving your Private collections on your next publish.`
										: `Delete "${g.name}"? Members fall back to Ungrouped.`}
									onconfirm={() => handleDeleteGroup(g)}
								/>
							</span>
						{/each}
						<button type="button" class="trusted-chip trusted-chip-add" onclick={() => (createGroupOpen = true)}>+ New group</button>
					</div>
				</div>
			{/if}

			{#if sections.length === 0 && online.length === 0}
				{#if searchQuery.trim()}
					<div class="empty">No contacts match "{searchQuery}".</div>
				{:else if filterTag}
					<div class="empty">No contacts with tag "{filterTag}".</div>
				{/if}
			{:else}
				{#if online.length > 0}
					<div class="phonebook-section">
						<div id="sec-online" class="section-header">● Online now</div>
						<div class="contact-list">
							{#each online as peer (peer.npub)}
								{@render contactRow(peer)}
							{/each}
						</div>
					</div>
				{/if}
				{#each sections as section (section.key)}
					<div class="phonebook-section">
						<div id={section.anchorId} class="section-header">{section.label}</div>
						<div class="contact-list">
							{#each section.peers as peer (peer.npub)}
								{@render contactRow(peer)}
							{/each}
						</div>
					</div>
				{/each}
			{/if}
		{/if}
	</div>
	{#if view === 'name'}
		<AZRail targets={railTargets} />
	{/if}
</div>
</div>
</div>

<CreateGroupDialog open={createGroupOpen} oncreate={handleCreateGroup} oncancel={() => (createGroupOpen = false)} />
<AddContactPanel
	open={addContactPanelOpen}
	onadd={openAddContact}
	onclose={() => (addContactPanelOpen = false)}
/>
<AddContactDialog
	open={addContactOpen}
	displayName={addContactDisplayName}
	{groups}
	onsave={handleAddContactSave}
	onskip={handleAddContactSkip}
	onnewGroup={() => (createGroupOpen = true)}
	oncancel={() => { addContactOpen = false; addContactTarget = null; }}
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

	/* Sticky sub-header: search + view toggle + add-contact — outside the scroll container. */
	.subheader {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 24px;
		border-bottom: 1px solid var(--border);
		background: var(--bg);
		flex-shrink: 0;
	}
	.subheader-search {
		flex: 1;
		max-width: 380px;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 0 11px;
		height: 34px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}
	.subheader-search .search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }
	.subheader-search input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 13px;
		color: var(--fg);
		min-width: 0;
	}
	.subheader-search input::placeholder { color: var(--fg-dim); }

	.view-toggle {
		display: flex;
		border: 1px solid var(--border);
		border-radius: 7px;
		overflow: hidden;
		flex-shrink: 0;
	}
	.view-toggle button {
		padding: 6px 12px;
		font-size: 12px;
		font-weight: 600;
		background: transparent;
		border: none;
		color: var(--fg-muted);
		cursor: pointer;
		font-family: var(--font-ui);
	}
	.view-toggle button[aria-pressed='true'] { background: var(--accent-soft); color: var(--accent); }

	/* Tag filter — collapsible, under the search bar, applied in both views. */
	.tagfilter-row { padding: 8px 24px 0; flex-shrink: 0; }
	.tagfilter-toggle {
		background: transparent; border: none; cursor: pointer;
		color: var(--fg-dim); font-size: 11px; font-weight: 500;
		display: inline-flex; align-items: center; gap: 4px;
		font-family: var(--font-ui); padding: 2px 0;
	}
	.tagfilter-toggle:hover { color: var(--fg-muted); }
	.tagfilter-chevron { display: flex; transition: transform 0.15s; }
	.tagfilter-chevron.open { transform: rotate(180deg); }

	.section-label {
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 600;
	}

	/* M10 — trusted-groups strip + private-collection display */
	.trusted-groups { padding: 8px 0 16px; display: flex; flex-direction: column; gap: 6px; }
	.trusted-label { font-size: 11.5px; font-weight: 600; color: var(--fg-muted); }
	.trusted-hint { font-weight: 400; color: var(--fg-dim); }
	.trusted-chips { display: flex; flex-wrap: wrap; gap: 6px; }
	.trusted-chip-wrap { display: inline-flex; align-items: center; gap: 2px; }
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

	/* Phonebook: scrollable section list + a fixed A-Z rail sibling. */
	.phonebook { display: flex; min-height: 0; flex: 1; max-width: 760px; }
	.phonebook-scroll { flex: 1; overflow-y: auto; padding: 16px 24px 24px; min-width: 0; }

	.phonebook-section { margin-bottom: 4px; }
	.section-header {
		position: sticky; top: 0; z-index: 2;
		background: var(--bg);
		padding: 6px 0;
		font-size: 10.5px; color: var(--fg-dim);
		text-transform: uppercase; letter-spacing: 1.2px; font-weight: 700;
		scroll-margin-top: 0;
	}
	#sec-online.section-header { color: var(--online); }

	/* Contacts list */
	.empty { color: var(--fg-dim); font-size: 13px; padding: 16px 0; }

	.contact-list { display: flex; flex-direction: column; gap: 12px; padding-bottom: 16px; }

	.contact-block { display: flex; flex-direction: column; gap: 8px; scroll-margin-top: 34px; }

	.contact-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 10px 12px;
		display: flex;
		gap: 10px;
		align-items: flex-start;
	}

	/* M15 W5: chevron toggles the light-detail area; ⋯ opens the row menu. */
	.chevron-btn {
		background: transparent; border: none; cursor: pointer; padding: 2px;
		display: flex; align-items: center; color: var(--fg-muted); flex-shrink: 0; margin-top: 5px;
	}
	.chevron { display: flex; transition: transform 0.15s; }
	.chevron-open { transform: rotate(180deg); }
	.row-menu-btn {
		width: 26px; height: 26px; flex-shrink: 0;
		display: flex; align-items: center; justify-content: center;
		background: transparent; border: 1px solid transparent; border-radius: 6px;
		color: var(--fg-muted); font-size: 15px; line-height: 1; cursor: pointer;
	}
	.row-menu-btn:hover { background: var(--bg-elev3); border-color: var(--border); color: var(--fg); }
	.menu-item {
		display: flex; align-items: center; width: 100%; text-align: left;
		padding: 7px 10px; font-family: var(--font-ui); font-size: 12.5px; color: var(--fg);
		background: transparent; border: none; border-radius: 5px; cursor: pointer;
	}
	.menu-item:hover:not(:disabled) { background: var(--bg-elev3); }
	.menu-item:disabled { opacity: 0.6; cursor: default; }
	.menu-item-confirm { padding: 3px 6px; }

	.contact-info { flex: 1; min-width: 0; }

	.name-row { display: flex; gap: 8px; align-items: center; margin-bottom: 3px; flex-wrap: wrap; }
	.peer-name { font-weight: 600; font-size: 15px; letter-spacing: -0.2px; }
	.mono { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	.last-seen { font-size: 10.5px; color: var(--fg-dim); }

	.contact-sub-row {
		display: flex; align-items: center; gap: 5px;
		margin-top: 2px; font-size: 11px; color: var(--fg-muted);
	}
	.sub-dot { color: var(--fg-dim); }
	.sub-meta { color: var(--fg-muted); font-feature-settings: 'tnum'; }

	/* Browse-key access (devtest #1/#6) — a 🔒 overlay on the avatar for a keyless (locked) contact
	   replaces the old inline "key needed"/"browseable" text badge; keyed contacts get no marker. */
	.avatar-wrap { position: relative; flex-shrink: 0; margin-top: 2px; }
	.lock-overlay {
		position: absolute; right: -4px; bottom: -4px;
		font-size: 10px; line-height: 1;
		padding: 2px; border-radius: 999px;
		background: var(--bg-elev1);
		box-shadow: 0 0 0 1px var(--border);
	}
	.access-hint { font-size: 10.5px; color: var(--fg-dim); margin-top: 2px; }

	/* Contact-card bio (devtest #7) — render-only, clamped to 2 lines. */
	.card-bio {
		font-size: 12px; color: var(--fg-muted); line-height: 1.5;
		margin: 4px 0 0;
		overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical;
	}

	/* Content-type badges + profile tags */
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

	/* M15 W7: removed the dead .modal-* block (unreferenced — grep-confirmed). */

	/* Tag filter bar */
	.tag-filter-row { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 0; padding: 0 24px; }
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

	.contact-detail { padding-left: 56px; padding-bottom: 4px; display: flex; flex-direction: column; gap: 8px; }

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

	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */
</style>
