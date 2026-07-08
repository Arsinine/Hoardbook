<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { toast, contacts } from '$lib/stores.js';
	import {
		topicList,
		topicCreate,
		topicDiscover,
		topicJoinPublic,
		topicRedeemInvite,
		topicLeave,
		topicInvite,
		topicRoster,
		topicAnnounce,
		topicAnnounceStatus,
	} from '$lib/api.js';
	import type { TopicView, DiscoveredTopic } from '$lib/types.js';
	import { memberCountLabel, rosterLabel, TOPIC_ROOTS, composeTopicPath, subPathLabel, groupTopicsByRoot } from '$lib/topics-view.js';
	import { canAnnounce, cooldownLabel, ANNOUNCE_EXPLAINER } from '$lib/announce-view.js';
	import TopicJoinConsent from '$lib/components/TopicJoinConsent.svelte';
	import HintMarker from '$lib/components/HintMarker.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';

	// Redesign (devtest 2026-06-25 #9): master–detail (My Topics list ↔ selected-topic detail),
	// Create as a modal + Discover as a tab (forms are no longer always-on stacked cards), and the
	// chat channel is a deep-link (its content lives in Chat since M11). Owner-chosen layout.
	let tab: 'mine' | 'discover' = $state('mine');
	let createOpen = $state(false);

	let mine: TopicView[] = $state([]);
	let discovered: DiscoveredTopic[] = $state([]);
	let busy = $state(false);

	// Create form. W4: a PUBLIC Topic is a category root (picker — a bad root is unrepresentable) + a
	// freeform sub-path (e.g. video / animation/anime). A PRIVATE Topic keeps a freeform name.
	let newRoot: string = $state(TOPIC_ROOTS[0]);
	let newSubPath = $state('');
	let newName = $state(''); // private (freeform) name
	let newDesc = $state('');
	let newTags = $state('');
	let newPrivate = $state(false);
	// The composed public path, previewed under the inputs.
	let composedPublicName = $derived(composeTopicPath(newRoot, newSubPath));
	let discoveredTree = $derived(groupTopicsByRoot(discovered));

	// Discover
	let searchTags = $state('');

	// The consent gate: which Topic (public name + private flag) is pending a join.
	let pendingJoin: { name: string; isPrivate: boolean } | null = $state(null);

	// Open Topic (roster + invite). The 24h channel now lives in Chat (a persistent channel entry per
	// joined Topic); posting moved there, so this panel keeps only membership management.
	let openTopic: TopicView | null = $state(null);
	let roster: string[] = $state([]);
	let inviteNpub = $state('');

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

	function splitTags(s: string): string[] {
		return s.split(',').map((t) => t.trim()).filter(Boolean);
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

	async function create() {
		if (!canCreate) return;
		busy = true;
		try {
			await topicCreate(createName, newDesc.trim(), splitTags(newTags), newPrivate);
			newName = newSubPath = newDesc = newTags = '';
			newRoot = TOPIC_ROOTS[0];
			newPrivate = false;
			createOpen = false;
			tab = 'mine';
			await loadMine();
			toast('Topic created', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function discover() {
		const tags = splitTags(searchTags);
		if (tags.length === 0) {
			toast('Enter at least one tag to discover Topics', 'error');
			return;
		}
		busy = true;
		try {
			discovered = await topicDiscover(tags);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
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
							{#if openTopic.description}<div class="muted">{openTopic.description}</div>{/if}
						</div>
						<ConfirmButton label="Leave" confirmText="Leave this Topic?" onconfirm={() => openTopic && leave(openTopic)} />
					</div>

					<div class="detail-section">
						<div class="section-label">Roster ({roster.length})</div>
						<ul class="roster">
							{#each roster as npub (npub)}<li>{rosterLabel(npub, $contacts)}</li>{/each}
						</ul>
					</div>

					<div class="invite">
						<input placeholder="invite an npub…" bind:value={inviteNpub} />
						<button onclick={invite}>Invite</button>
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

					<a class="channel-link" href="/chat">💬 Open this Topic’s channel in Chat →</a>
				{:else}
					<div class="detail-empty">Select a Topic to see its roster, invite members, and open its chat channel.</div>
				{/if}
			</div>
		</section>
	{:else}
		<!-- Discover tab -->
		<section class="discover-tab">
			<div class="discover-controls">
				<input class="grow" placeholder="search tags, comma-separated" bind:value={searchTags} onkeydown={(e) => e.key === 'Enter' && discover()} />
				<button class="btn-primary" disabled={busy} onclick={discover}>Discover</button>
			</div>
			<!-- W4: results render as a tree split on '/' (root category → sub-paths), activity-ranked
			     within each root by the backend. -->
			{#if discoveredTree.length === 0}
				<p class="muted empty">Enter one or more tags, then press Discover to find public Topics.</p>
			{:else}
				{#each discoveredTree as group (group.root)}
					<div class="tree-root">{group.root}</div>
					{#each group.topics as d (d.topic_id)}
						<div class="row tree-child">
							<div class="grow">
								<div class="name">{subPathLabel(d.name) || d.name}</div>
								<div class="muted">{memberCountLabel(d.member_count_estimate)}</div>
							</div>
							<button onclick={() => askToJoin(d.name, false)}>Join</button>
						</div>
					{/each}
				{/each}
			{/if}
			<button class="link" disabled={busy} onclick={redeemInvite}>Redeem a private Topic invite</button>
		</section>
	{/if}
</div>

<!-- Create-a-Topic modal (devtest #9: was an always-on card; now invoked from "+ New Topic"). -->
{#if createOpen}
	<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
	<div class="modal-backdrop" role="dialog" aria-modal="true" aria-label="Create a Topic" tabindex="-1" onclick={(e) => { if (e.target === e.currentTarget) createOpen = false; }}>
		<div class="modal">
			<h2>New Topic</h2>
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
			<input placeholder="tags, comma-separated" bind:value={newTags} />
			<label class="check"><input type="checkbox" bind:checked={newPrivate} /> Private (unlisted)</label>
			<div class="modal-actions">
				<button class="ghost" onclick={() => (createOpen = false)}>Cancel</button>
				<button class="btn-primary" disabled={busy || !canCreate} onclick={create}>Create</button>
			</div>
		</div>
	</div>
{/if}

<!-- F12 consent gate: a join (public or private) fires only after explicit acknowledgment. -->
{#if pendingJoin}
	<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
	<div
		class="modal-backdrop"
		role="dialog"
		aria-modal="true"
		aria-label="Join Topic consent"
		tabindex="-1"
		onclick={(e) => { if (e.target === e.currentTarget) pendingJoin = null; }}
	>
		<div class="modal">
			<h2>Join “{pendingJoin.name}”</h2>
			<TopicJoinConsent
				isPrivate={pendingJoin.isPrivate}
				disabled={busy}
				onjoin={confirmJoin}
				oncancel={() => (pendingJoin = null)}
			/>
		</div>
	</div>
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

	/* Discover tab */
	.discover-tab {
		flex: 1; min-height: 0; overflow-y: auto;
		background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 10px; padding: 16px;
		display: flex; flex-direction: column; gap: 8px;
	}
	.discover-controls { display: flex; gap: 8px; }

	.empty { padding: 16px 8px; }

	/* Shared controls */
	input {
		padding: 6px 9px; background: var(--bg-elev2); color: var(--fg);
		border: 1px solid var(--border); border-radius: 6px; font: inherit;
	}
	button {
		padding: 6px 12px; border-radius: 6px; border: 1px solid var(--border);
		background: var(--bg-elev2); color: var(--fg); font: inherit; cursor: pointer;
	}
	button:disabled { opacity: 0.5; cursor: not-allowed; }
	.btn-primary { background: var(--accent); color: var(--accent-text); border-color: var(--accent); font-weight: 600; }
	button.ghost, button.link { background: transparent; }
	button.link { border: none; color: var(--accent); text-align: left; padding: 4px 0; margin-top: 4px; }
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
	.tree-root { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.4px; color: var(--fg-dim); margin-top: 8px; }
	.tree-child { padding-left: 12px; }
	.roster { list-style: none; margin: 0; padding: 0; font-size: 12px; max-height: 200px; overflow-y: auto; }
	.roster li { padding: 3px 0; }
	.invite { display: flex; gap: 6px; }
	.invite input { flex: 1; }
	.announce-row { display: flex; gap: 6px; margin-top: 2px; }
	.announce-row input { flex: 1; }
	.channel-link { display: inline-block; margin-top: 4px; font-size: 12px; color: var(--accent); text-decoration: none; }
	.channel-link:hover { text-decoration: underline; }
	.modal-backdrop {
		position: fixed; inset: 0; z-index: 9998;
		background: oklch(0 0 0 / 0.45);
		display: flex; align-items: center; justify-content: center;
	}
	.modal {
		background: var(--bg-elev1);
		border: 1px solid var(--border-strong);
		border-radius: 12px;
		padding: 18px;
		width: min(440px, 90vw);
		display: flex; flex-direction: column; gap: 8px;
	}
	.modal h2 { font-size: 14px; font-weight: 700; margin: 0 0 6px; }
	.modal-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
</style>
