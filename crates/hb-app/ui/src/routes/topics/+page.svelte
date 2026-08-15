<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { toast, contacts, identity, profile, topicAnnounceSummaries, announceSeen } from '$lib/stores.js';
	import {
		topicList,
		topicCreate,
		topicUpdateMeta,
		topicDiscover,
		topicLookup,
		topicJoinPublic,
		topicRedeemInvite,
		topicPreviewInvite,
		topicLeave,
		topicInvite,
		topicRoster,
		topicAnnounce,
		topicAnnounceStatus,
	} from '$lib/api.js';
	import type { TopicView, DiscoveredTopic, TopicLookup, CachedPeer } from '$lib/types.js';
	import { memberCountLabel, rosterLabel, unseenTopicAnnouncements, TOPIC_ROOTS, composeTopicPath, subPathLabel, createPrimaryAction } from '$lib/topics-view.js';
	import { canAnnounce, cooldownLabel, ANNOUNCE_EXPLAINER } from '$lib/announce-view.js';
	import { icons } from '$lib/icons.js';
	import TopicJoinConsent from '$lib/components/TopicJoinConsent.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import HintMarker from '$lib/components/HintMarker.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';
	import ContactPicker from '$lib/components/ContactPicker.svelte';
	import PersonRow from '$lib/components/PersonRow.svelte';

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
	// QURATOR-80: a fetch FAILURE must not render as the confident negative "No public Topics under X
	// yet" — that string is indistinguishable from a genuine empty, a timeout, an empty relay set, or
	// an untagged announce. Track which roots FAILED so the surface can say "we could not reach the
	// relays" (retryable) vs "the relays answered, there are none" (not). An error is NOT cached: a
	// re-expand retries. Nor is a successful EMPTY (QURATOR-83) — only a non-empty list is cached.
	let erroredRoots: Set<string> = $state(new Set());
	// Request-generation guard for Discover, same shape as `lookupGeneration` below. A fetch already
	// IN FLIGHT when a Topic is created would otherwise resolve afterwards and cache its PRE-PUBLISH
	// result: at eviction time the key is still `undefined`, so deleting it is a no-op, and the late
	// resolve then writes a stale NON-EMPTY list — which `toggleRoot` treats as terminal, hiding the
	// user's own new Topic until restart. That is QURATOR-83 again by another route (codex). Bumped
	// on every public create; a resolving fetch applies its result only if its generation still holds.
	let discoverGeneration = 0;

	// The consent gate: which Topic (public name + private flag) is pending a join. For a private
	// redeem, `issuerNpub` carries who vouches (surfaced in the consent modal); `mode: 'redeem'` routes
	// the confirm button to `confirmRedeem` (the commit) instead of `confirmJoin` (the public path).
	let pendingJoin:
		| { name: string; isPrivate: boolean; mode: 'join'; issuerNpub?: string }
		| { name: string; isPrivate: boolean; mode: 'redeem'; issuerNpub: string; topicId: string }
		| null = $state(null);

	// Open Topic (roster + invite). The 24h channel now lives in Chat (a persistent channel entry per
	// joined Topic); posting moved there, so this panel keeps only membership management.
	let openTopic: TopicView | null = $state(null);
	let roster: string[] = $state([]);
	// M21 W3: Invite opens the ContactPicker modal (select a contact OR type a new npub) instead of a
	// bare inline text field.
	let invitePickerOpen = $state(false);

	// Hoardbook Topics draft r1 — unread pill: the per-row twin of the Chat nav badge's announce
	// share. The data is already polled app-wide (+layout.svelte); this is a pure render addition —
	// no new fetch. A topic is "unseen" when its latest announcement is past its seen watermark; the
	// watermark still advances only in Chat (topicAnnounceMarkSeen), which clears the pill here via
	// the shared `announceSeen` store.
	let unseenTopics = $derived(new Set(unseenTopicAnnouncements($topicAnnounceSummaries, $announceSeen).map((s) => s.topic_id)));

	// Hoardbook Topics draft r1 — Discover search: filters ONLY across roots already expanded and
	// successfully cached (pure client-side over already-fetched data). Deliberately NO fetch-on-
	// search: `toggleRoot`'s fetch/cache/retry machine is pinned by three call-count tests
	// (QURATOR-80/83/85), and an eager fetch here would break them. Coverage is therefore partial by
	// design and says so (the hint below) rather than pretending to be full.
	let searchQuery = $state('');

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
			const createdName = createName;
			const createdPrivate = newPrivate;
			await topicCreate(createdName, newDesc.trim(), createdPrivate);
			// QURATOR-83: a new public Topic in a root invalidates that root's cached Discover result so
			// the next expand re-fetches (otherwise a previously-empty root stays empty until restart).
			// A private Topic is unlisted, so no root is affected. Delete the single key by constructing a
			// new object explicitly — a cached empty for this root is the stale value being evicted here.
			if (!createdPrivate) {
				const root = createdName.split('/')[0];
				// Bump FIRST and unconditionally: a fetch in flight right now has a key that is still
				// `undefined`, so the deletion below cannot reach it — only the generation can.
				discoverGeneration += 1;
				if (root && rootTopics[root] !== undefined) {
					const next: Record<string, DiscoveredTopic[]> = {};
					for (const k in rootTopics) if (k !== root) next[k] = rootTopics[k];
					rootTopics = next;
				}
			}
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
		// A successful fetch caches NON-EMPTY results (the cache exists so switching roots isn't a
		// relay round-trip each time). A cached genuine EMPTY (`[]`) is NOT terminal — QURATOR-83: an
		// empty found before a Topic existed was cached forever, swallowing every later create in that
		// root until restart. Now an empty is a miss: re-expand re-fetches. A FAILED fetch is NOT
		// cached either: it lands in `erroredRoots` instead (QURATOR-80), so a re-expand retries.
		const cached = rootTopics[root];
		if ((cached !== undefined && cached.length > 0) || erroredRoots.has(root)) {
			if (erroredRoots.has(root)) {
				// Re-expand on an errored root = explicit retry: clear the error and re-fetch.
				const next = new Set(erroredRoots);
				next.delete(root);
				erroredRoots = next;
			} else {
				return; // cached non-empty result — a genuine empty falls through to re-fetch
			}
		}
		loadingRoot = root;
		const generation = discoverGeneration;
		try {
			const found = await topicDiscover([root]);
			// Drop the result if a Topic was published while this was in flight — it predates the
			// publish, and caching it would re-hide the user's own Topic. The key stays absent, so the
			// next expand re-fetches.
			if (generation === discoverGeneration) {
				rootTopics = { ...rootTopics, [root]: found }; // cache regardless — keyed by root
				// QURATOR-85: a successful fetch clears a stale error on this root — the relays just
				// answered, so "we could not reach them" is no longer true. Without this, an overlapping
				// FAILED request can leave erroredRoots set AFTER a SUCCESSFUL one caches real topics, and
				// the template checks the error branch first, hiding the list.
				if (erroredRoots.has(root)) {
					const next = new Set(erroredRoots);
					next.delete(root);
					erroredRoots = next;
				}
			}
		} catch (e) {
			// Only surface if this root is STILL the open one: a stale request for a category the user
			// already switched away from must not error over the current one (codex). Keep the section
			// expanded with an error status (distinct from the genuine-empty message) so the user can
			// retry by collapsing + re-expanding — do NOT collapse to a confident negative.
			if (expandedRoot === root) {
				toast(String(e), 'error');
				const next = new Set(erroredRoots);
				next.add(root);
				erroredRoots = next;
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
		pendingJoin = { name, isPrivate, mode: 'join' };
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

	// W8: redeem is consent-gated like the public join. The preview reveals the invite issuer + topic
	// name WITHOUT committing; the follow-up `confirmRedeem` calls the commit only after the ack.
	async function redeemInvite() {
		busy = true;
		try {
			const preview = await topicPreviewInvite();
			if (!preview) {
				toast('No pending invite found', 'success');
				return;
			}
			// Open the consent modal carrying the issuer npub + topic name — do NOT commit yet. The
			// previewed topic_id is carried onto pendingJoin so confirmRedeem can bind the redeem to it
			// (W8 substitution guard): a relay that serves a different valid invite at redeem is rejected.
			pendingJoin = {
				name: preview.name,
				isPrivate: true,
				mode: 'redeem',
				issuerNpub: preview.issuer_npub,
				topicId: preview.topic_id
			};
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function confirmRedeem() {
		if (!pendingJoin) return;
		// Only reached in 'redeem' mode (the consent modal routes here, not to confirmJoin), so
		// pendingJoin.topicId is present — narrow with the mode check to keep svelte-check happy.
		if (pendingJoin.mode !== 'redeem') return;
		busy = true;
		try {
			const joined = await topicRedeemInvite(pendingJoin.topicId);
			if (joined) {
				pendingJoin = null;
				tab = 'mine';
				await loadMine();
				toast(`Joined private Topic “${joined.name}”`, 'success');
			} else {
				pendingJoin = null;
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

	// M21 W3: ContactPicker emits the chosen npub (a selected contact OR a typed new one); this routes
	// straight through the same `topicInvite` command as the old inline field.
	async function inviteChosen(npub: string) {
		if (!openTopic || !npub) return;
		invitePickerOpen = false;
		try {
			await topicInvite(openTopic.topic_id, npub);
			toast('Invite sent', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// Hoardbook Topics draft r1 — roster row resolution: the roster carries bare npubs, and the row's
	// picture / fingerprint / presence come from the contacts store when the npub is a saved contact
	// ("you" is implicitly online). A non-contact has none of these — PersonRow omits the fingerprint
	// line and the presence dot rather than guessing a state (absent-gracefully, the M21 W4
	// behaviour-4 precedent: "no ring, no word row"). The name stays `rosterLabel`'s (self → petname
	// → short-npub), so nothing here re-derives a display name.
	function rosterRowProps(npub: string): {
		name: string;
		letter: string;
		picture?: string;
		fingerprint?: CachedPeer['fingerprint'];
		online: boolean;
	} {
		const self = $identity ? { npub: $identity.npub, display_name: $profile?.display_name } : null;
		const isSelf = self !== null && npub === self.npub;
		const contact = $contacts.find((c) => c.npub === npub);
		const name = rosterLabel(npub, $contacts, self);
		return {
			name,
			letter: name[0]?.toUpperCase() ?? '?',
			picture: contact?.profile?.picture,
			fingerprint: contact?.fingerprint,
			online: isSelf || contact?.online === true,
		};
	}

	// Hoardbook Topics draft r1 — Discover search corpus: every root that has been successfully
	// fetched (a key in `rootTopics`), path-only per the draft's own choice (the name / sub-path
	// label, never the description). Roots never expanded are simply not in the corpus, and the
	// template says so instead of pretending full coverage. Pure array filtering — no fetches, no
	// new state machine (`toggleRoot`'s cache/retry machine is pinned by the QURATOR-80/83/85
	// call-count tests and stays untouched).
	let searchResults = $derived.by(() => {
		const q = searchQuery.trim().toLowerCase();
		if (!q) return [] as DiscoveredTopic[];
		// Codex review (2026-08-15): topic_discover is a TAG query (crates/hb-app/src/commands/
		// topics.rs), not a strict name-prefix lookup, so the same topic_id can legitimately surface
		// under more than one root's cache (e.g. a legacy/externally published announce carrying
		// multiple root-category tags). Dedupe by topic_id — first-seen wins — or the `(d.topic_id)`
		// keyed #each below renders duplicate keys.
		const seen = new Map<string, DiscoveredTopic>();
		for (const root of TOPIC_ROOTS) {
			for (const d of rootTopics[root] ?? []) {
				if (
					!seen.has(d.topic_id) &&
					(d.name.toLowerCase().includes(q) || subPathLabel(d.name).toLowerCase().includes(q))
				) {
					seen.set(d.topic_id, d);
				}
			}
		}
		return [...seen.values()];
	});
	// Partial-coverage hint: true while at least one root category has never been fetched, so the
	// search can only speak for the expanded subset.
	let searchCoveragePartial = $derived(TOPIC_ROOTS.some((root) => rootTopics[root] === undefined));
</script>

<!-- TopBar — the shared app shell (see routes/+page.svelte, contacts, settings). -->
<!-- QURATOR-81 follow-up — see contacts/+page.svelte: the topbar is the drag handle. The attribute
     does not inherit, so the tabs and "+ New Topic" inside keep their clicks. -->
<div class="topbar" data-tauri-drag-region>
	<div>
		<div class="topbar-title">Topics</div>
		<div class="topbar-sub">
			{mine.length} Topic{mine.length !== 1 ? 's' : ''} joined
		</div>
	</div>
	<div class="topbar-actions">
		<div class="tabs">
			<button class="tab" class:tab-active={tab === 'mine'} onclick={() => (tab = 'mine')}>My Topics</button>
			<button class="tab" class:tab-active={tab === 'discover'} onclick={() => (tab = 'discover')}>Discover</button>
		</div>
		<button class="btn-primary" onclick={() => (createOpen = true)}>+ New Topic</button>
	</div>
</div>

<div class="body">
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
							<!-- Hoardbook Topics draft r1 — unread pill: this topic's announcement is past its
							     seen watermark (boolean only; no per-topic count exists in this data shape).
							     Mirrors the Chat nav-badge's visual language (+layout.svelte .nav-badge). -->
							{#if unseenTopics.has(t.topic_id)}
								<span class="unread" title="New announcement"></span>
							{/if}
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
									<input class="hb-input grow" bind:value={descDraft} placeholder="description" onkeydown={(e) => e.key === 'Enter' && saveDesc()} />
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
							{#each roster as npub (npub)}
								{@const row = rosterRowProps(npub)}
								<li><PersonRow name={row.name} letter={row.letter} picture={row.picture} fingerprint={row.fingerprint} online={row.online} /></li>
							{/each}
						</ul>
					</div>

					<div class="invite">
						<button class="btn-default" onclick={() => (invitePickerOpen = true)}>Invite</button>
					</div>

					<!-- M13 Part A (Q1) — sends only; the announce itself renders in the Chat topic thread. -->
					<div class="detail-section">
						<div class="section-label">
							Announce to members
							<HintMarker text={ANNOUNCE_EXPLAINER} label="announce to members" />
						</div>
						<div class="announce-row">
							<input
								class="hb-input grow"
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
						<!-- Hoardbook Topics draft r1 — the terms are visible without hovering: the 24h/one-per-
						     hour limits shouldn't be hidden behind the HintMarker tooltip (kept for the "?"
						     affordance). The constant itself is unchanged; announce-view.test.ts pins it. -->
						<div class="announce-terms">{ANNOUNCE_EXPLAINER}</div>
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
			<!-- Hoardbook Topics draft r1 — search across the already-fetched roots only. No fetch-on-
			     search: toggleRoot's fetch/cache/retry machine is pinned by the QURATOR-80/83/85
			     call-count tests and stays untouched, so typing never fires topicDiscover. -->
			<div class="discover-search">
				<input
					class="hb-input"
					type="search"
					placeholder="Search expanded categories…"
					bind:value={searchQuery}
					aria-label="Search expanded categories"
				/>
				{#if searchQuery.trim()}
					{#if searchCoveragePartial}
						<span class="search-hint muted">Expand a category to search it too</span>
					{:else}
						<span class="search-hint muted">{searchResults.length} match{searchResults.length === 1 ? '' : 'es'} across all categories</span>
					{/if}
				{/if}
			</div>
			{#if searchQuery.trim()}
				<!-- Filtered results replace the accordion while a query is live (the categories are the
				     corpus being searched). Path-only per the draft's own choice — descriptions are not
				     searched. Rows reuse the accordion's own row markup + Join action. -->
				<div class="search-results">
					{#if searchResults.length === 0}
						<div class="root-status muted">No public Topics matching “{searchQuery.trim()}” in the expanded categories.</div>
					{:else}
						{#each searchResults as d (d.topic_id)}
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
				</div>
			{:else}
				{#each TOPIC_ROOTS as root (root)}
					<div class="root-group">
						<button class="root-header" onclick={() => toggleRoot(root)} aria-expanded={expandedRoot === root}>
							<span class="root-chevron" class:open={expandedRoot === root}>{@html icons.chevronRight}</span>
							<span class="root-name">{root}</span>
						</button>
						{#if expandedRoot === root}
							{#if loadingRoot === root}
								<div class="root-status muted">Loading…</div>
							{:else if erroredRoots.has(root)}
								<!-- QURATOR-80: a fetch failure is NOT the confident negative "No public Topics under X
								     yet" — that string is indistinguishable from a genuine empty. This surface reads "we
								     could not reach the relays" (retryable) so an unknown is never rendered as a confident
								     negative. Collapsing + re-expanding retries (toggleRoot clears the error and re-fetches). -->
								<div class="root-status root-error">
									Couldn’t reach the relays for “{root}”. Collapse and re-expand to retry.
								</div>
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
			{/if}
			<button class="link" disabled={busy} onclick={redeemInvite}>Redeem a private Topic invite</button>
		</section>
	{/if}
</div>

<!-- Create-a-Topic modal (devtest #9: was an always-on card; now invoked from "+ New Topic"). -->
<Modal open={createOpen} title="New Topic" onclose={() => (createOpen = false)}>
	<div class="create-fields">
		{#if newPrivate}
			<input class="hb-input" placeholder="name (freeform, e.g. back room)" bind:value={newName} />
		{:else}
			<!-- W4: a public Topic is a category root (picker) + freeform sub-path. The root picker
			     makes a non-category root unrepresentable; the backend re-validates authoritatively. -->
			<div class="path-row">
				<select class="root-pick" bind:value={newRoot}>
					{#each TOPIC_ROOTS as r}<option value={r}>{r}</option>{/each}
				</select>
				<span class="path-sep">/</span>
				<input class="hb-input grow" placeholder="sub-path (e.g. animation/anime) — optional" bind:value={newSubPath} />
			</div>
			<div class="muted path-preview">Topic path: <code>{composedPublicName}</code></div>
		{/if}
		<input class="hb-input" placeholder="description" bind:value={newDesc} />
		<label class="check"><input type="checkbox" bind:checked={newPrivate} /> Private (unlisted)</label>
	</div>
	{#snippet actions()}
		<button class="btn-ghost" onclick={() => (createOpen = false)}>Cancel</button>
		<button class="btn-primary" disabled={busy || !canCreate} onclick={handlePrimary}>{primaryAction.label}</button>
	{/snippet}
</Modal>

<!-- F12 consent gate: a join (public or private) or a redeem fires only after explicit acknowledgment. -->
{#if pendingJoin}
	<Modal open={true} title={`Join “${pendingJoin.name}”`} onclose={() => (pendingJoin = null)}>
		<TopicJoinConsent
			isPrivate={pendingJoin.isPrivate}
			issuerNpub={pendingJoin.mode === 'redeem' ? pendingJoin.issuerNpub : ''}
			disabled={busy}
			onjoin={pendingJoin.mode === 'redeem' ? confirmRedeem : confirmJoin}
			oncancel={() => (pendingJoin = null)}
		/>
	</Modal>
{/if}

<!-- M21 W3 — Invite opens a ContactPicker (pick a contact OR type a new npub). Stacked above the
     detail pane; single-select because topicInvite takes exactly one npub per call (api.ts). -->
<ContactPicker
	open={invitePickerOpen}
	title="Invite to Topic"
	confirmLabel="Invite"
	contacts={$contacts}
	myNpub={$identity?.npub ?? ''}
	onselect={inviteChosen}
	onclose={() => (invitePickerOpen = false)}
/>

<style>
	/* Shared app shell — same rules as routes/+page.svelte, contacts and settings. */
	.topbar {
		padding: 16px 24px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 16px;
		background: var(--bg);
		flex-shrink: 0;
	}
	.topbar-title { font-size: 17px; font-weight: 600; color: var(--fg); letter-spacing: -0.3px; }
	.topbar-sub { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }
	.topbar-actions { display: flex; gap: 8px; align-items: center; }

	.body { flex: 1; min-height: 0; padding: 18px 22px; box-sizing: border-box; display: flex; flex-direction: column; }

	.tabs { display: inline-flex; background: var(--bg-elev1); border: 1px solid var(--border); border-radius: 8px; padding: 2px; }
	.tab {
		padding: 5px 12px; border: none; background: transparent; color: var(--fg-muted);
		font: inherit; font-size: 12.5px; border-radius: 6px; cursor: pointer;
	}
	/* M20 W4: converged to the accent-soft form (DESIGN_SYSTEM §3 "Segmented toggle") — matches
	   Contacts' Name|Groups toggle exactly, replacing the off-system elev3 fill. */
	.tab-active { background: var(--accent-soft); color: var(--accent); font-weight: 600; }

	/* Master–detail — flat, matching Browse's .left-panel / Chat's .convo-sidebar: a divider, not a
	   box. (Previously wrapped in a shared `.surface` card with a 16px gap between panes; that made
	   Topics the only master-detail page boxed like Settings/Contacts instead of flat like its
	   actual layout siblings — stripped 2026-08-15.) */
	.master-detail { display: flex; gap: 0; align-items: stretch; flex: 1; min-height: 0; }
	.list-pane {
		width: 280px; flex-shrink: 0; overflow-y: auto; padding: 6px;
		border-right: 1px solid var(--border);
	}
	.topic-row {
		display: flex; align-items: center; gap: 8px; width: 100%; text-align: left;
		padding: 9px 10px; background: transparent; border: none; border-radius: 7px; cursor: pointer; color: inherit;
	}
	.topic-row:hover { background: var(--bg-elev2); }
	.topic-selected { background: var(--bg-elev3); }
	.detail-pane {
		flex: 1; min-width: 0; overflow-y: auto; padding: 16px;
		display: flex; flex-direction: column; gap: 12px;
	}
	.detail-head { display: flex; align-items: flex-start; gap: 8px; }
	.detail-title { font-size: 15px; font-weight: 700; }
	.detail-section { display: flex; flex-direction: column; gap: 4px; }
	/* Same ramp as contacts/settings `.section-label` (10.5px / 1.2px / 600). */
	.section-label { font-size: 10.5px; font-weight: 600; text-transform: uppercase; letter-spacing: 1.2px; color: var(--fg-dim); }
	.detail-empty { color: var(--fg-dim); font-size: 12.5px; margin: auto; text-align: center; max-width: 280px; }

	/* Discover tab — devtest v0.12.1 #7: primitive (root category) accordion. */
	.discover-tab {
		flex: 1; min-height: 0; overflow-y: auto; padding: 16px;
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
	/* QURATOR-80: the fetch-failure status is on the shared --error ramp (same token as .btn-danger),
	   so an unknown reads visually distinct from the muted genuine-empty status, not as a confident
	   negative. jsdom computes no layout — this colour is asserted in the source-scan test, not
	   rendered by vitest. */
	.root-error { color: var(--error); }

	.empty { padding: 16px 8px; }

	/* Shared controls — M20 W4: inputs use the global .hb-input contract (app.css); the bare
	   `input {}` element selector is gone (it filled --bg-elev2 and leaked onto modal fields). */
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
	.announce-row { display: flex; gap: 6px; margin-top: 2px; }
	.announce-row input { flex: 1; }
	/* Hoardbook Topics draft r1 — the announce terms as an always-visible caption (a .dest-style
	   small line: same 11.5px / --fg-dim ramp the page's .muted captions use). */
	.announce-terms { font-size: 11.5px; color: var(--fg-dim); margin-top: 4px; }
	/* Hoardbook Topics draft r1 — unread pill: the per-row announce share, on the nav-badge's accent
	   fill (boolean only — this data shape carries no per-topic count). */
	.unread {
		width: 8px; height: 8px; border-radius: 999px; flex-shrink: 0;
		background: var(--accent); box-shadow: 0 0 0 1px var(--bg-elev1);
	}
	/* Hoardbook Topics draft r1 — Discover search (filters the already-fetched roots; never fetches). */
	.discover-search { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
	.discover-search input { flex: 1; }
	.search-hint { flex-shrink: 0; font-size: 11px; }
	.search-results { display: flex; flex-direction: column; }
	.channel-link { display: inline-block; margin-top: 4px; font-size: 12px; color: var(--accent); text-decoration: none; }
	.channel-link:hover { text-decoration: underline; }
	/* M15 W2: the two Topic modals now use Modal.svelte; only the create form's field spacing is local. */
	.create-fields { display: flex; flex-direction: column; gap: 8px; }
</style>
