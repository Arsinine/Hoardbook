<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { toast, contacts, identity, profile } from '$lib/stores.js';
	import {
		topicList,
		topicCreate,
		topicUpdateMeta,
		topicDiscover,
		topicLookup,
		topicJoinPublic,
		topicRedeemInvite,
		topicLeave,
		topicInvite,
		topicRoster,
		topicAnnounce,
		topicAnnounceStatus,
	} from '$lib/api.js';
	import type { TopicView, DiscoveredTopic, TopicLookup } from '$lib/types.js';
	import { memberCountLabel, rosterLabel, TOPIC_ROOTS, composeTopicPath, subPathLabel, createPrimaryAction } from '$lib/topics-view.js';
	import { canAnnounce, cooldownLabel, ANNOUNCE_EXPLAINER } from '$lib/announce-view.js';
	import { icons } from '$lib/icons.js';
	import TopicJoinConsent from '$lib/components/TopicJoinConsent.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import HintMarker from '$lib/components/HintMarker.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';

	// Redesign (devtest 2026-06-25 #9): master–detail (My Topics list ↔ selected-topic detail),
	// Create as a modal + Discover as a tab (forms are no longer always-on stacked cards), and the
	// chat channel is a deep-link (its content lives in Chat since M11). Owner-chosen layout.
	let tab: 'mine' | 'discover' = $state('mine');
	let createOpen = $state(false);

	let mine: TopicView[] = $state([]);
	let busy = $state(false);

	// Create form. W4: a PUBLIC Topic is a category root (picker — a bad root is unrepresentable) + a
	// freeform sub-path (e.g. video / animation/anime). A PRIVATE Topic keeps a freeform name.
	let newRoot: string = $state(TOPIC_ROOTS[0]);
	let newSubPath = $state('');
	let newName = $state(''); // private (freeform) name
	let newDesc = $state('');
	let newPrivate = $state(false);
	// The composed public path, previewed under the inputs.
	let composedPublicName = $derived(composeTopicPath(newRoot, newSubPath));

	// devtest v0.12.1 #7: Discover-by-primitive — the six root categories, each expandable to every
	// public Topic under it (no tag search). Results are fetched lazily on first expand + cached.
	let expandedRoot: string | null = $state(null);
	let rootTopics: Record<string, DiscoveredTopic[]> = $state({});
	let loadingRoot: string | null = $state(null);

	// The consent gate: which Topic (public name + private flag) is pending a join.
	let pendingJoin: { name: string; isPrivate: boolean } | null = $state(null);

	// Open Topic (roster + invite). The 24h channel now lives in Chat (a persistent channel entry per
	// joined Topic); posting moved there, so this panel keeps only membership management.
	let openTopic: TopicView | null = $state(null);
	let roster: string[] = $state([]);
	let inviteNpub = $state('');

	// devtest v0.12.1 #8: a Topic's description is editable after creation (the name is immutable).
	let editingDesc = $state(false);
	let descDraft = $state('');
	let savingDesc = $state(false);

	// M13 Part A (Q1) — this page only SENDS an announce; the announce list itself renders in the Chat
	// topic thread. `announceRemaining` seeds from the backend on open() and ticks down locally every
	// 60s (a coarse local countdown only — a rejection re-syncs it from the authoritative backend).
	let announceBody = $state('');
	let announceRemaining = $state(0);
	let announcing = $state(false);
	let announceTicker: ReturnType<typeof setInterval> | undefined;

	onDestroy(() => { if (announceTicker) clearInterval(announceTicker); });

	async function sendAnnounce() {
		if (!openTopic || !announceBody.trim() || !canAnnounce(announceRemaining) || announcing) return;
		announcing = true;
		const body = announceBody.trim();
		try {
			await topicAnnounce(openTopic.topic_id, body);
			announceBody = '';
			toast('Announcement sent', 'success');
			announceRemaining = await topicAnnounceStatus(openTopic.topic_id);
		} catch (e) {
			toast(String(e), 'error');
			// The backend is authoritative on rejection (e.g. still cooling down) — re-sync, don't trust
			// the locally-ticked value.
			if (openTopic) {
				try { announceRemaining = await topicAnnounceStatus(openTopic.topic_id); } catch { /* keep last */ }
			}
		} finally {
			announcing = false;
		}
	}

	async function loadMine() {
		try {
			mine = await topicList();
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	onMount(loadMine);

	// The effective name to create: a freeform private name, or the composed category path for public.
	let createName = $derived(newPrivate ? newName.trim() : composedPublicName);
	let canCreate = $derived(newPrivate ? newName.trim().length > 0 : composedPublicName.length > 0);

	// devtest #11 — join-first: before minting a new PUBLIC Topic, check (debounced) whether its
	// composed name already has a room. A private Topic never looks up (no announce exists to find).
	let topicNameLookup: TopicLookup | null = $state(null);
	let lookupTimer: ReturnType<typeof setTimeout> | undefined;
	// Request-generation guard: a stale response landing after a newer one (or after the name changed
	// again) must not overwrite the fresher result — e.g. typing "existing name" then a fresh name
	// could otherwise let the older "exists: true" response land last, leaving a Join button that
	// fails. Bumped every time a lookup is (re)scheduled; a resolving lookup applies its result only
	// if its captured generation still matches.
	let lookupGeneration = 0;

	$effect(() => {
		const name = composedPublicName;
		// Clear immediately on any input change — pending state defaults to Create, never a stale
		// Join carried over from a previous name.
		topicNameLookup = null;
		lookupGeneration += 1;
		const generation = lookupGeneration;
		if (newPrivate || !name) {
			return;
		}
		clearTimeout(lookupTimer);
		lookupTimer = setTimeout(async () => {
			let result: TopicLookup | null;
			try {
				result = await topicLookup(name);
			} catch {
				result = null; // best-effort — a failed lookup just falls back to Create
			}
			if (generation === lookupGeneration) {
				topicNameLookup = result;
			}
		}, 300);
	});
	onDestroy(() => clearTimeout(lookupTimer));

	// The Create modal's primary action: Create by default, or Join when the composed public name
	// already has a room (a same-name public Topic must not fork into a second, distinct room).
	let primaryAction = $derived(createPrimaryAction(topicNameLookup));

	async function handlePrimary() {
		if (!canCreate) return;
		if (primaryAction.mode === 'join') {
			createOpen = false;
			askToJoin(createName, false);
			return;
		}
		await create();
	}

	async function create() {
		if (!canCreate) return;
		busy = true;
		try {
			await topicCreate(createName, newDesc.trim(), newPrivate);
			newName = newSubPath = newDesc = '';
			newRoot = TOPIC_ROOTS[0];
			newPrivate = false;
			createOpen = false;
			tab = 'mine';
			await loadMine();
			toast('Topic created', 'success');
		} catch (e) {
			const msg = String(e);
			toast(msg, 'error');
			// The backend rechecks for an existing announce immediately before publish (the UI's
			// join-first lookup above is only a preflight, not airtight against a race between two
			// clients). Re-run the lookup so the primary action flips to Join — never auto-join, the
			// consent gate (F12) still requires an explicit click.
			if (!newPrivate && msg.includes('already exists')) {
				try {
					topicNameLookup = await topicLookup(composedPublicName);
				} catch {
					topicNameLookup = null;
				}
			}
		} finally {
			busy = false;
		}
	}

	// devtest v0.12.1 #7: expand a primitive (root category) to list every public Topic under it. The
	// per-root fetch is lazy (first expand) + cached; the backend activity-ranks and caps the results.
	async function toggleRoot(root: string) {
		if (expandedRoot === root) {
			expandedRoot = null;
			return;
		}
		expandedRoot = root;
		if (rootTopics[root]) return; // already fetched
		loadingRoot = root;
		try {
			const found = await topicDiscover([root]);
			rootTopics = { ...rootTopics, [root]: found }; // cache regardless — keyed by root
		} catch (e) {
			// Only surface/collapse if this root is STILL the open one: a stale request for a category
			// the user already switched away from must not error over or collapse the current one (codex).
			if (expandedRoot === root) {
				toast(String(e), 'error');
				expandedRoot = null;
			}
		} finally {
			// Only clear the spinner if it belongs to THIS request — a stale resolve must not clear a
			// newer root's loading state (codex).
			if (loadingRoot === root) loadingRoot = null;
		}
	}

	// devtest v0.12.1 #8: edit the open Topic's description (name stays fixed — it derives the room id).
	function startEditDesc() {
		if (!openTopic) return;
		descDraft = openTopic.description;
		editingDesc = true;
	}

	async function saveDesc() {
		if (!openTopic || savingDesc) return;
		savingDesc = true;
		try {
			const updated = await topicUpdateMeta(openTopic.topic_id, descDraft.trim());
			openTopic = updated;
			mine = mine.map((t) => (t.topic_id === updated.topic_id ? updated : t));
			editingDesc = false;
			toast('Topic updated', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			savingDesc = false;
		}
	}

	// Join is always gated behind the consent component (F12) — even for public Topics.
	function askToJoin(name: string, isPrivate: boolean) {
		pendingJoin = { name, isPrivate };
	}

	async function confirmJoin() {
		if (!pendingJoin) return;
		busy = true;
		try {
			await topicJoinPublic(pendingJoin.name);
			pendingJoin = null;
			tab = 'mine';
			await loadMine();
			toast('Joined Topic', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function redeemInvite() {
		busy = true;
		try {
			const joined = await topicRedeemInvite();
			if (joined) {
				tab = 'mine';
				await loadMine();
				toast(`Joined private Topic “${joined.name}”`, 'success');
			} else {
				toast('No pending invite found', 'success');
			}
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function leave(t: TopicView) {
		busy = true;
		try {
			await topicLeave(t.topic_id);
			if (openTopic?.topic_id === t.topic_id) {
				openTopic = null;
				if (announceTicker) clearInterval(announceTicker);
			}
			await loadMine();
			toast('Left Topic', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function open(t: TopicView) {
		openTopic = t;
		roster = [];
		announceBody = '';
		editingDesc = false;
		if (announceTicker) clearInterval(announceTicker);
		try {
			roster = await topicRoster(t.topic_id);
		} catch (e) {
			toast(String(e), 'error');
		}
		try {
			announceRemaining = await topicAnnounceStatus(t.topic_id);
		} catch {
			announceRemaining = 0;
		}
		announceTicker = setInterval(() => {
			announceRemaining = Math.max(0, announceRemaining - 60);
		}, 60_000);
	}

	async function invite() {
		if (!openTopic || !inviteNpub.trim()) return;
		try {
			await topicInvite(openTopic.topic_id, inviteNpub.trim());
			inviteNpub = '';
			toast('Invite sent', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}
</script>

<div class="page">
	<header>
		<h1>Topics</h1>
		<div class="header-actions">
			<div class="tabs">
				<button class="tab" class:tab-active={tab === 'mine'} onclick={() => (tab = 'mine')}>My Topics</button>
				<button class="tab" class:tab-active={tab === 'discover'} onclick={() => (tab = 'discover')}>Discover</button>
			</div>
			<button class="btn-primary" onclick={() => (createOpen = true)}>+ New Topic</button>
		</div>
	</header>

	{#if tab === 'mine'}
		<section class="master-detail">
			<!-- Left: My Topics list -->
			<div class="list-pane">
				{#if mine.length === 0}
					<p class="muted empty">You haven’t joined any Topics yet. Create one, or switch to Discover.</p>
				{:else}
					{#each mine as t (t.topic_id)}
						<button class="topic-row" class:topic-selected={openTopic?.topic_id === t.topic_id} onclick={() => open(t)}>
							<div class="grow">
								<div class="name">{t.name} {#if t.private}<span class="tag">private</span>{/if}</div>
								{#if t.description}<div class="muted">{t.description}</div>{/if}
							</div>
						</button>
					{/each}
				{/if}
			</div>

			<!-- Right: detail (roster + invite + chat deep-link) -->
			<div class="detail-pane">
				{#if openTopic}
					<div class="detail-head">
						<div class="grow">
							<div class="detail-title">{openTopic.name} {#if openTopic.private}<span class="tag">private</span>{/if}</div>
							<!-- devtest v0.12.1 #8: description is editable after creation (the name is not). -->
							{#if editingDesc}
								<div class="desc-edit">
									<input class="grow" bind:value={descDraft} placeholder="description" onkeydown={(e) => e.key === 'Enter' && saveDesc()} />
									<button class="btn-primary btn-sm" disabled={savingDesc} onclick={saveDesc}>{savingDesc ? '…' : 'Save'}</button>
									<button class="btn-ghost btn-sm" disabled={savingDesc} onclick={() => (editingDesc = false)}>Cancel</button>
								</div>
							{:else}
								<div class="desc-row">
									{#if openTopic.description}<span class="muted">{openTopic.description}</span>{:else}<span class="muted desc-empty">No description</span>{/if}
									<button class="desc-edit-btn" onclick={startEditDesc}>Edit</button>
								</div>
							{/if}
						</div>
						<ConfirmButton label="Leave" confirmText="Leave this Topic?" onconfirm={() => openTopic && leave(openTopic)} />
					</div>

					<div class="detail-section">
						<div class="section-label">Roster ({roster.length})</div>
						<ul class="roster">
							{#each roster as npub (npub)}<li>{rosterLabel(npub, $contacts, $identity ? { npub: $identity.npub, display_name: $profile?.display_name } : null)}</li>{/each}
						</ul>
					</div>

					<div class="invite">
						<input placeholder="invite an npub…" bind:value={inviteNpub} />
						<button class="btn-default" onclick={invite}>Invite</button>
					</div>

					<!-- M13 Part A (Q1) — sends only; the announce itself renders in the Chat topic thread. -->
					<div class="detail-section">
						<div class="section-label">
							Announce to members
							<HintMarker text={ANNOUNCE_EXPLAINER} label="announce to members" />
						</div>
						<div class="announce-row">
							<input
								class="grow"
								placeholder="a highlighted notice for all members…"
								bind:value={announceBody}
								onkeydown={(e) => e.key === 'Enter' && sendAnnounce()}
								disabled={!canAnnounce(announceRemaining) || announcing}
							/>
							<button
								class="btn-primary"
								disabled={!announceBody.trim() || !canAnnounce(announceRemaining) || announcing}
								onclick={sendAnnounce}
							>
								{announcing ? '…' : cooldownLabel(announceRemaining)}
							</button>
						</div>
					</div>

					<a class="channel-link" href="/chat?topic={openTopic.topic_id}">💬 Open this Topic’s channel in Chat →</a>
				{:else}
					<div class="detail-empty">Select a Topic to see its roster, invite members, and open its chat channel.</div>
				{/if}
			</div>
		</section>
	{:else}
		<!-- Discover tab — devtest v0.12.1 #7: browse public Topics by primitive (root category). No tag
		     search; expand a category to fetch every public Topic under it (backend activity-ranked). -->
		<section class="discover-tab">
			<p class="muted discover-hint">Browse public Topics by category. Expand one to see every public Topic under it.</p>
			{#each TOPIC_ROOTS as root (root)}
				<div class="root-group">
					<button class="root-header" onclick={() => toggleRoot(root)} aria-expanded={expandedRoot === root}>
						<span class="root-chevron" class:open={expandedRoot === root}>{@html icons.chevronRight}</span>
						<span class="root-name">{root}</span>
					</button>
					{#if expandedRoot === root}
						{#if loadingRoot === root}
							<div class="root-status muted">Loading…</div>
						{:else if (rootTopics[root] ?? []).length === 0}
							<div class="root-status muted">No public Topics under “{root}” yet.</div>
						{:else}
							{#each rootTopics[root] as d (d.topic_id)}
								<div class="row tree-child">
									<div class="grow">
										<div class="name">{subPathLabel(d.name) || d.name}</div>
										{#if d.description}<div class="muted">{d.description}</div>{/if}
										<div class="muted">{memberCountLabel(d.member_count_estimate)}</div>
									</div>
									<button class="btn-default" onclick={() => askToJoin(d.name, false)}>Join</button>
								</div>
							{/each}
						{/if}
					{/if}
				</div>
			{/each}
			<button class="link" disabled={busy} onclick={redeemInvite}>Redeem a private Topic invite</button>
		</section>
	{/if}
</div>

<!-- Create-a-Topic modal (devtest #9: was an always-on card; now invoked from "+ New Topic"). -->
<Modal open={createOpen} title="New Topic" onclose={() => (createOpen = false)}>
	<div class="create-fields">
		{#if newPrivate}
			<input placeholder="name (freeform, e.g. back room)" bind:value={newName} />
		{:else}
			<!-- W4: a public Topic is a category root (picker) + freeform sub-path. The root picker
			     makes a non-category root unrepresentable; the backend re-validates authoritatively. -->
			<div class="path-row">
				<select class="root-pick" bind:value={newRoot}>
					{#each TOPIC_ROOTS as r}<option value={r}>{r}</option>{/each}
				</select>
				<span class="path-sep">/</span>
				<input class="grow" placeholder="sub-path (e.g. animation/anime) — optional" bind:value={newSubPath} />
			</div>
			<div class="muted path-preview">Topic path: <code>{composedPublicName}</code></div>
		{/if}
		<input placeholder="description" bind:value={newDesc} />
		<label class="check"><input type="checkbox" bind:checked={newPrivate} /> Private (unlisted)</label>
	</div>
	{#snippet actions()}
		<button class="btn-ghost" onclick={() => (createOpen = false)}>Cancel</button>
		<button class="btn-primary" disabled={busy || !canCreate} onclick={handlePrimary}>{primaryAction.label}</button>
	{/snippet}
</Modal>

<!-- F12 consent gate: a join (public or private) fires only after explicit acknowledgment. -->
{#if pendingJoin}
	<Modal open={true} title={`Join “${pendingJoin.name}”`} onclose={() => (pendingJoin = null)}>
		<TopicJoinConsent
			isPrivate={pendingJoin.isPrivate}
			disabled={busy}
			onjoin={confirmJoin}
			oncancel={() => (pendingJoin = null)}
		/>
	</Modal>
{/if}

<style>
	.page { padding: 18px 22px; overflow-y: auto; height: 100%; box-sizing: border-box; display: flex; flex-direction: column; }
	header { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 14px; }
	header h1 { font-size: 18px; font-weight: 700; margin: 0; }
	.header-actions { display: flex; align-items: center; gap: 10px; }
	.tabs { display: inline-flex; background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 8px; padding: 2px; }
	.tab {
		padding: 5px 12px; border: none; background: transparent; color: var(--fg-muted);
		font: inherit; font-size: 12.5px; border-radius: 6px; cursor: pointer;
	}
	.tab-active { background: var(--bg-elev3); color: var(--fg); font-weight: 600; }

	/* Master–detail */
	.master-detail { display: flex; gap: 16px; align-items: stretch; flex: 1; min-height: 0; }
	.list-pane {
		width: 280px; flex-shrink: 0; overflow-y: auto;
		background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 10px; padding: 6px;
	}
	.topic-row {
		display: flex; align-items: center; gap: 8px; width: 100%; text-align: left;
		padding: 9px 10px; background: transparent; border: none; border-radius: 7px; cursor: pointer; color: inherit;
	}
	.topic-row:hover { background: var(--bg-elev2); }
	.topic-selected { background: var(--bg-elev3); }
	.detail-pane {
		flex: 1; min-width: 0; overflow-y: auto;
		background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 10px; padding: 16px;
		display: flex; flex-direction: column; gap: 12px;
	}
	.detail-head { display: flex; align-items: flex-start; gap: 8px; }
	.detail-title { font-size: 15px; font-weight: 700; }
	.detail-section { display: flex; flex-direction: column; gap: 4px; }
	.section-label { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.4px; color: var(--fg-dim); }
	.detail-empty { color: var(--fg-dim); font-size: 12.5px; margin: auto; text-align: center; max-width: 280px; }

	/* Discover tab — devtest v0.12.1 #7: primitive (root category) accordion. */
	.discover-tab {
		flex: 1; min-height: 0; overflow-y: auto;
		background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 10px; padding: 16px;
		display: flex; flex-direction: column; gap: 4px;
	}
	.discover-hint { margin-bottom: 6px; }
	.root-group { border-top: 1px solid var(--divider); }
	.root-group:first-of-type { border-top: none; }
	.root-header {
		display: flex; align-items: center; gap: 8px; width: 100%; text-align: left;
		padding: 9px 4px; background: transparent; border: none; cursor: pointer; color: var(--fg);
		font: inherit; font-size: 13px; font-weight: 600; text-transform: capitalize;
	}
	.root-header:hover { color: var(--accent); }
	.root-chevron { display: flex; transition: transform 0.15s; color: var(--fg-dim); }
	.root-chevron.open { transform: rotate(90deg); }
	.root-status { padding: 6px 0 6px 22px; }

	.empty { padding: 16px 8px; }

	/* Shared controls */
	input {
		padding: 6px 9px; background: var(--bg-elev2); color: var(--fg);
		border: 1px solid var(--border); border-radius: 6px; font: inherit;
	}
	/* M15 W1: buttons unified on the app.css .btn system. `.link` stays a local text-link (no boxed
	   equivalent in the shared system); `button:disabled` keeps the .tab/.link dim state. */
	button:disabled { opacity: 0.5; cursor: not-allowed; }
	button.link { background: transparent; border: none; color: var(--accent); text-align: left; padding: 4px 0; margin-top: 4px; cursor: pointer; }
	.check { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--fg-muted); }
	.grow { flex: 1; min-width: 0; }
	.row { display: flex; align-items: center; gap: 8px; padding: 6px 0; border-top: 1px solid var(--divider); }
	.name { font-size: 13px; font-weight: 600; }
	.muted { font-size: 11.5px; color: var(--fg-dim); }
	.tag { font-size: 10px; color: var(--accent); border: 1px solid var(--border); border-radius: 4px; padding: 0 4px; }
	.path-row { display: flex; align-items: center; gap: 6px; }
	.root-pick { padding: 6px 9px; background: var(--bg-elev2); color: var(--fg); border: 1px solid var(--border); border-radius: 6px; font: inherit; }
	.path-sep { color: var(--fg-dim); }
	.path-preview { font-size: 11px; }
	.path-preview code { font-family: var(--font-mono); color: var(--fg-muted); }
	.tree-child { padding-left: 22px; }
	/* devtest v0.12.1 #8: inline description edit in the detail head. */
	.desc-row { display: flex; align-items: baseline; gap: 8px; }
	.desc-empty { font-style: italic; }
	.desc-edit-btn {
		background: transparent; border: none; cursor: pointer; color: var(--accent);
		font: inherit; font-size: 11px; padding: 0; flex-shrink: 0;
	}
	.desc-edit-btn:hover { text-decoration: underline; }
	.desc-edit { display: flex; align-items: center; gap: 6px; margin-top: 4px; }
	.desc-edit input { flex: 1; }
	.roster { list-style: none; margin: 0; padding: 0; font-size: 12px; max-height: 200px; overflow-y: auto; }
	.roster li { padding: 3px 0; }
	.invite { display: flex; gap: 6px; }
	.invite input { flex: 1; }
	.announce-row { display: flex; gap: 6px; margin-top: 2px; }
	.announce-row input { flex: 1; }
	.channel-link { display: inline-block; margin-top: 4px; font-size: 12px; color: var(--accent); text-decoration: none; }
	.channel-link:hover { text-decoration: underline; }
	/* M15 W2: the two Topic modals now use Modal.svelte; only the create form's field spacing is local. */
	.create-fields { display: flex; flex-direction: column; gap: 8px; }
</style>
