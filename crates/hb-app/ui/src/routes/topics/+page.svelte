<script lang="ts">
	import { onMount, onDestroy, tick } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { toast, contacts, identity, profile, topicAnnounceSummaries, announceSeen, topicDirectoryCache } from '$lib/stores.js';
	import {
		pasteKey,
		topicList,
		topicCreate,
		topicUpdateMeta,
		topicDiscoverPaint,
		topicRank,
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
	import {
		rosterLabel,
		unseenTopicAnnouncements,
		TOPIC_ROOTS,
		composeTopicPath,
		topicRootOf,
		subPathLabel,
		createPrimaryAction,
		interleaveRoundRobin,
		orderByMemberCount,
		TOPIC_GROUP_DRAW_CAP,
		groupTopicsByRoot,
		memberCountLabel,
	} from '$lib/topics-view.js';
	import { canAnnounce, cooldownLabel, ANNOUNCE_EXPLAINER } from '$lib/announce-view.js';
	import { icons } from '$lib/icons.js';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import TopicJoinConsent from '$lib/components/TopicJoinConsent.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import HintMarker from '$lib/components/HintMarker.svelte';
	import ConfirmButton from '$lib/components/ConfirmButton.svelte';
	import ContactPicker from '$lib/components/ContactPicker.svelte';
	import PersonRow from '$lib/components/PersonRow.svelte';

	// Redesign (devtest 2026-06-25 #9) → QURATOR-89 r3 W2: the master–detail shell keeps its shape
	// but the master IS the directory now — every announced public Topic, grouped by path root,
	// joined rows in white with the admin meta line, unjoined rows muted with their blurb. There is
	// no My/Discover tab split anymore: the room list is the directory, painted by one relay read.
	let createOpen = $state(false);

	let mine: TopicView[] = $state([]);
	let busy = $state(false);
	// QURATOR-93: loadMine used to toast its failure and then fall through to the template, whose
	// `mine.length === 0` branch rendered the confident "You haven't joined any Topics yet" negative
	// on data that never arrived. Same rule as the tree error below: a FAILED load is a separate
	// state; a later successful loadMine clears it (a stale error never hides a good list).
	let mineLoadError = $state(false);

	// Create form. W4: a Topic is a category root (picker — a bad root is unrepresentable) + a
	// freeform sub-path (e.g. video / animation/anime). QURATOR-147 W5: PRIVATE Topics use the same
	// picker — the owner ruling is that private obeys the public path convention, and before W5 the
	// form offered private only a free-text name, so obeying meant knowing to type `video/foo` by
	// hand (which is why legacy private Topics look like `back room`). The backend keeps a private
	// name freeform and verbatim; `other` is the root for anything that wants a bare word.
	let newRoot: string = $state(TOPIC_ROOTS[0]);
	let newSubPath = $state('');
	let newDesc = $state('');
	let newPrivate = $state(false);
	// The composed path (root + sub-path), previewed under the inputs. Same for public and private.
	let composedName = $derived(composeTopicPath(newRoot, newSubPath));

	// QURATOR-144 W2 — the whole directory lives in the left pane on open. One paint fetch fills it;
	// there is no per-root cache/retry machine anymore (that was the tab split's machine). Its
	// QURATOR-80/85 error contract survives as ONE tree-level state: a failed paint is a retryable
	// error, never the confident negative "no public Topics under X yet" — one error for the whole
	// tree where there used to be six independent per-root ones.
	let painted = $state(false);
	let painting = $state(false);
	let paintError = $state(false);
	// QURATOR-83, carried into W2: a fetch already IN FLIGHT when a Topic is created would resolve
	// afterwards and cache its PRE-PUBLISH result, hiding the user's own new Topic until restart.
	// Bumped on every public create; a resolving paint applies its result only if its generation
	// still holds.
	let discoverGeneration = 0;
	// The painted half of the directory (announced public Topics, counts null until ranked).
	let directory: DiscoveredTopic[] = $state([]);

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
	// Finding #36 (CWE-362): request-generation guard for the open-Topic panel, same shape as
	// `lookupGeneration`/`discoverGeneration` below. Clicking Topic A then Topic B leaves A's async
	// roster (and announce status) in flight; without a guard the later resolve binds A's roster to
	// B's open pane, over-disclosing to the wrong audience. Bumped on every open(); a resolving
	// fetch applies its result only if its captured generation still holds.
	let openGeneration = 0;
	// M21 W3: Invite opens the ContactPicker modal (select a contact OR type a new npub) instead of a
	// bare inline text field.
	let invitePickerOpen = $state(false);

	// QURATOR-144 W2 — one selection drives one detail pane, and the row can now be an UNJOINED
	// directory row (a grey row is never disabled: selecting it shows what a non-member can honestly
	// be shown — blurb + claimed count + Join, never the roster). A joined selection re-uses `open`
	// (which owns the roster fetch + generation guard); an unjoined selection only carries the id.
	let selectedDiscoveredId: string | null = $state(null);
	// The claimed count for the SELECTED unjoined Topic, fetched lazily on selection (never in the
	// list — r4: the sidebar orders by counts, it never displays them; the detail pane is where a
	// count is allowed to appear).
	let selectedClaimed: number | null = $state(null);
	// Which root groups the user has collapsed. A group starts OPEN if you are joined to something
	// under it; pure-discovery groups start collapsed — modelled as a collapsed SET seeded from that
	// rule (not an open set), so the default is computable and the user's toggles are remembered.
	let collapsedRoots = $state(new Set<string>());

	// Hoardbook Topics draft r1 — unread pill: the per-row twin of the Chat nav badge's announce
	// share. The data is already polled app-wide (+layout.svelte); this is a pure render addition —
	// no new fetch. A topic is "unseen" when its latest announcement is past its seen watermark; the
	// watermark still advances only in Chat (topicAnnounceMarkSeen), which clears the pill here via
	// the shared `announceSeen` store.
	let unseenTopics = $derived(new Set(unseenTopicAnnouncements($topicAnnounceSummaries, $announceSeen).map((s) => s.topic_id)));

	// QURATOR-144 W2 — the filter matches PATHS ONLY (owner ruling): every match is then
	// self-evident in the row label. Matching groups are force-opened while a query is live.
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

	// The expand-rank coalescing timer (declared with toggleGroup below, hoisted here so the
	// unmount cleanup can see it): a pending window that outlives the component would fire
	// rankDrawnRows against a destroyed tree.
	let expandRankTimer: ReturnType<typeof setTimeout> | undefined;
	onDestroy(() => {
		if (announceTicker) clearInterval(announceTicker);
		if (expandRankTimer) clearTimeout(expandRankTimer);
	});

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
			const next = await topicList();
			// QURATOR-149: the JOIN TRANSITION opens the group, not a permanent override. `had` is
			// read from the OLD `mine` before the assignment; a root that just GAINED a joined Topic
			// un-collapses here (the original rationale, narrowed to the instant it was about: "a
			// toggle collapsing it then a join landing"). `isCollapsed` no longer consults
			// `rootsWithJoined`, so the chevron stays a live control afterwards — including in the
			// groups the user actually works in.
			const had = new Set(mine.map((t) => topicRootOf(t.name)));
			mine = next;
			mineLoadError = false;
			const gained = new Set(next.map((t) => topicRootOf(t.name)).filter((r) => !had.has(r)));
			if (gained.size > 0) collapsedRoots = new Set([...collapsedRoots].filter((r) => !gained.has(r)));
		} catch (e) {
			toast(String(e), 'error');
			mineLoadError = true;
		}
	}

	// QURATOR-145 (W3) — paint the CACHED tree instantly, refresh behind it. The cache is the
	// last-known-good directory (see `topicDirectoryCache` in stores.ts): non-empty on arrival
	// here means a populated tree, so the landing screen is instant and a slightly-stale list is
	// harmless (W2's auto-population made it the landing screen, not something you went clicking
	// for). An EMPTY cache paints nothing — there is no honest "instant" screen for a nothing we
	// never had, and the fetch alone fills the pane (the QURATOR-80/83 rule: a cached NOTHING is
	// indistinguishable from the feature being broken, so an empty is never cached at all).
	// `painted` STAYS FALSE with a cached paint: this is a landing screen, not an answer — the
	// background fetch still fires below and its result replaces `directory` as normal. The
	// cached tree renders in PAINT order only, with NO eager rankDrawnRows: ranking fires when
	// the fresh answer lands (the paint path's own call), and an eager pass on the cached rows
	// would both double the open-time rank calls and mark those ids ranked, suppressing the fresh
	// answer's rank pass over the same ids (rankedIds is page state the fresh paint inherits).
	onMount(() => {
		const cached = get(topicDirectoryCache);
		if (cached.length > 0) {
			directory = cached;
		}
		void loadMine();
		void paintDirectory();
	});

	// The effective name to create: the composed category path (public and private alike — W5).
	let createName = $derived(composedName);
	let canCreate = $derived(composedName.length > 0);

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
		const name = composedName;
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
			// QURATOR-83, W2 form: a new PUBLIC Topic must be visible in the directory without a
			// restart. There is no per-root cache to evict anymore — the whole tree re-paints (one
			// read). A private Topic is unlisted, so the directory is unaffected.
			if (!createdPrivate) {
				// Bump FIRST and unconditionally: a paint in flight right now can only be stopped by
				// the generation — there is no cache key whose deletion would reach it.
				discoverGeneration += 1;
				painted = false;
				void paintDirectory();
			}
			newSubPath = newDesc = '';
			newRoot = TOPIC_ROOTS[0];
			newPrivate = false;
			createOpen = false;
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
					topicNameLookup = await topicLookup(composedName);
				} catch {
					topicNameLookup = null;
				}
			}
		} finally {
			busy = false;
		}
	}

	// QURATOR-143 W1 + QURATOR-144 W2 — the ONE-READ paint. Fires the single whole-directory query
	// when the page opens (and after a public create re-dirties the tree) and fills the merged
	// sidebar's directory half. Before W1 this was six per-root `topicDiscover` calls, each followed
	// by a member_count round trip per topic found — ~600 relay round trips per page open, the exact
	// burst the M16 relay-citizenship ruling forbids. Now: one read, no counts.
	async function paintDirectory() {
		if (painted || painting) return;
		painting = true;
		const generation = discoverGeneration;
		let superseded = false;
		try {
			const all = await topicDiscoverPaint([...TOPIC_ROOTS]);
			// Dedupe by topic_id across roots (Codex 2026-08-15): the same topic can legitimately
			// surface under several roots' tags — first seen wins, so the keyed #each never renders
			// duplicate keys.
			const seen = new Set<string>();
			const uniq: DiscoveredTopic[] = [];
			for (const d of all) {
				if (seen.has(d.topic_id)) continue;
				seen.add(d.topic_id);
				uniq.push(d);
			}
			if (generation !== discoverGeneration) {
				// A public create landed mid-flight: this answer predates it. The create's own
				// paintDirectory() call bailed on the `painting` guard, so this resolve must chain
				// the re-read itself (QURATOR-83 — the user's own Topic must not stay hidden).
				superseded = true;
			} else {
				directory = uniq;
				// QURATOR-151 — the fresh answer carries `member_count_estimate: null` for every row
				// (the paint half fetches no counts), so any id in `rankedIds` is now a stale "its
				// count is on screen" claim: the repaint just wiped it. Same treatment as the W3
				// cache-paint gives the OPEN path — an inherited rankedIds would both hide the lost
				// counts and suppress the re-fetch (the rank filter trusts the set), degrading the
				// group's popularity order to paint order for the rest of the session.
				rankedIds = new Set();
				deadDropped = 0;
				painted = true;
				paintError = false;
				// QURATOR-145 (W3): only a NON-EMPTY answer enters the cross-mount cache. An empty
				// is never cached (a cached NOTHING is indistinguishable from broken — the exact
				// QURATOR-80/83 bug this workstream exists to not recreate at ten times the
				// visibility), and a FAILURE writes nothing (the catch below), so the next open
				// always re-asks and the last-known-good tree survives to degrade to. The screen
				// still takes the fresh EMPTY as truth (`directory = uniq` above) — only the CACHE
				// is protected from it.
				if (uniq.length > 0) topicDirectoryCache.set(uniq);
				rankGeneration += 1;
				void rankDrawnRows(rankGeneration);
			}
		} catch (e) {
			// QURATOR-80, one-tree form: a failed paint is a retryable error, NEVER the confident
			// negative "no public Topics" — the surface says "we could not reach the relays" and
			// keeps the retry affordance. `painted` stays false, so the Retry button re-paints.
			// QURATOR-145 (W3): a failure also never touches the cross-mount cache, and it never
			// blanks `directory` — whatever is on screen (cached-and-painted, or a fresh tree)
			// STAYS on screen: degrade to last-known, never blank. `paintError` only renders the
			// error branch when the tree is EMPTY (the template's `mergedRows.length === 0`
			// guard), so a populated screen never swaps itself for the error surface either.
			paintError = true;
			toast(String(e), 'error');
		} finally {
			painting = false;
		}
		if (superseded) await paintDirectory();
	}

	// QURATOR-143 W1 — the lazy ranker: fetch member_count ONLY for rows that will actually be drawn
	// (per-group cap + joined rows + the selected row), round-robin across roots, then re-order each
	// group most-popular-first as the counts land. Bounded to concurrency 8 by hb-net (`topic_rank`);
	// this side bounds the REQUEST to the drawn rows, which is the other half of the bound.
	let rankedIds = $state(new Set<string>());
	// QURATOR-148: how many painted rows the aliveness fold has DROPPED (known-dead Topics). Drives
	// the honest all-quiet empty state above — reset on each fresh paint, like `rankedIds`.
	let deadDropped = $state(0);
	let rankGeneration = 0;
	async function rankDrawnRows(generation: number) {
		// Await the flush BEFORE consulting collapse state: this runs in the same turn that set
		// `directory`, and the group-seeding $effect below (which writes collapsedRoots) has not
		// run yet — reading isCollapsed now would see every group open.
		await tick();
		// Per-root queues of the ids that WILL be drawn, in draw order. Two exclusions, both because
		// a queued id is a spent relay read:
		//   — JOINED rows never enter the queue: the sidebar renders them from the local `mine`
		//     record (mergedRows hardcodes member_count_estimate null for the joined half), and the
		//     fold below only writes counts back into `directory`, so a joined id's result is
		//     discarded — and it would still displace an unjoined row from the group's capped batch.
		//   — COLLAPSED groups' rows wait: a queued-but-undrawn row is a read spent on nobody (GLM
		//     review: a fresh user with everything collapsed was firing up to 6×25 reads for
		//     invisible rows). The expand path ranks them lazily.
		const queues: string[][] = [];
		for (const root of TOPIC_ROOTS) {
			const rows = rowsForRoot(root);
			if (rows.length === 0) continue;
			if (isCollapsed(root, true)) continue; // not drawn — not ranked until expanded
			// The same visibleRows the template draws with — the queue is exactly what is drawn.
			const drawn = visibleRows(rows).drawn
				// A paint that already carried the count (a member's announce states one) IS the
				// ordering datum — re-asking topicRank for it is the redundant call W2 forbids.
				.filter((d) => !d.joined && !rankedIds.has(d.topic_id) && d.member_count_estimate === null);
			if (drawn.length > 0) queues.push(drawn.map((d) => d.topic_id));
		}
		if (queues.length === 0) return;
		// Round-robin interleave (r4: "never spend all budget on one root") — with two roots pending,
		// neither drains the other's slots: the first 8 ids cannot all come from one root.
		const ids = interleaveRoundRobin(queues);
		try {
			// QURATOR-148: each ranked row rides with its directory NAME — the backend derives the
			// public-join credential from it to recover a non-member's topic key for the aliveness
			// read. An id that somehow misses the directory sends '' (recovery skipped, aliveness
			// stays unknown — never a guessed name).
			const nameById = new Map(directory.map((d) => [d.topic_id, d.name]));
			const ranks = await topicRank(ids.map((id) => ({ topic_id: id, name: nameById.get(id) ?? '' })));
			if (generation !== rankGeneration) return; // a newer pass superseded this rank request
			// Fold the counts in: unknown stays unknown (null), never 0.
			const byId = new Map(ranks.map((r) => [r.topic_id, r.member_count_estimate]));
			// QURATOR-148: fold aliveness in alongside. A KNOWN zero (no member pinged in 30 days)
			// drops the row from the directory — "whether or not it's worth joining, meaning whether
			// or not it shows up as an option in the discovery sidebar". null is UNKNOWN: the row
			// stays (the same never-a-confident-zero rule the count obeys). The backend reads a
			// non-member row's roster via the name-derived public-join credential (the name rides
			// the rank request above), so an unjoined public row CAN come back as a known 0 here.
			const aliveById = new Map(ranks.map((r) => [r.topic_id, r.alive_count]));
			const dropped = directory.filter((d) => aliveById.get(d.topic_id) === 0).length;
			if (dropped > 0) deadDropped += dropped;
			directory = orderByMemberCount(
				directory
					.filter((d) => aliveById.get(d.topic_id) !== 0)
					.map((d) =>
						byId.has(d.topic_id) ? { ...d, member_count_estimate: byId.get(d.topic_id)! } : d,
					),
			);
			rankedIds = new Set([...rankedIds, ...ids]);
		} catch {
			/* ranking is best-effort ordering — the paint is the data; leave rows in paint order */
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
		// Same open-generation guard as `open()` (finding #36): the save targets the open Topic, so a
		// resolve landing after the user opened a different Topic must not clobber the new `openTopic`
		// with the stale one (or toast over the new context).
		const generation = openGeneration;
		try {
			const updated = await topicUpdateMeta(openTopic.topic_id, descDraft.trim());
			if (generation === openGeneration) {
				openTopic = updated;
				mine = mine.map((t) => (t.topic_id === updated.topic_id ? updated : t));
				editingDesc = false;
				toast('Topic updated', 'success');
			}
		} catch (e) {
			if (generation === openGeneration) toast(String(e), 'error');
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
		const name = pendingJoin.name;
		try {
			await topicJoinPublic(name);
			pendingJoin = null;
			// The join may have been confirmed from the DETAIL pane of that same Topic: the stale
			// unjoined selection would keep rendering "You're not a member yet" + a live Join for a
			// Topic just joined (`directory` still carries its announce until the next paint, and
			// `selectedDiscovered` derives from `directory`, so the stale view would win). Clear the
			// unjoined selection and re-derive the detail from the joined record — the same
			// joined-wins merge the sidebar row already flipped to.
			selectedDiscoveredId = null;
			selectedClaimed = null;
			await loadMine();
			const joined = mine.find((t) => t.name === name);
			if (joined) await open(joined);
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
		selectedDiscoveredId = null; // the two selection kinds are mutually exclusive
		roster = [];
		announceBody = '';
		editingDesc = false;
		if (announceTicker) clearInterval(announceTicker);
		// Finding #36 (CWE-362): bump + capture the open-generation token so a stale resolve from a
		// previously-open Topic (its roster/status still in flight) cannot bind to this one.
		openGeneration += 1;
		const generation = openGeneration;
		try {
			const fetched = await topicRoster(t.topic_id);
			if (generation === openGeneration) roster = fetched;
		} catch (e) {
			if (generation === openGeneration) toast(String(e), 'error');
		}
		try {
			const remaining = await topicAnnounceStatus(t.topic_id);
			if (generation === openGeneration) announceRemaining = remaining;
		} catch {
			if (generation === openGeneration) announceRemaining = 0;
		}
		if (generation === openGeneration) {
			announceTicker = setInterval(() => {
				announceRemaining = Math.max(0, announceRemaining - 60);
			}, 60_000);
		}
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
		isSelf: boolean;
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
			// QURATOR-146: the hand-off is for reaching OTHER people — you cannot DM yourself, and
			// pasteKey rejects your own code, so the self row stays a plain non-interactive row.
			isSelf,
		};
	}

	// ── QURATOR-146 (Topics W4) — the roster hand-off: an interest-first search ends at a list of
	// people, and a read-only list dead-ends the flow. Contacts and Topics are two paths to the same
	// end (owner, 2026-08-27), so the roster row hands off to Chat with the SAME gesture Contacts
	// already uses — double-click → `/chat?peer=<npub>`. No add-to-contacts button here: owner ruling
	// "adding someone directly from the topic makes no sense. Makes more sense to talk to them first
	// and then add them, from the chat page" — `upsert_topic_contact` stays unused by this page.
	// A gesture with no affordance is invisible to a first-time user, so the row is also a real
	// button: Enter on the focused row fires the same navigation, and the hover cue says what it does.
	function openRosterChat(npub: string) {
		// Svelte's SPA `goto` (same as Contacts' ondblclick) — no full page reload.
		goto('/chat?peer=' + encodeURIComponent(npub));
	}

	// QURATOR-146 — hover a member → their bio, "much like contacts". Contacts has NO hovercard: it
	// clamps the bio on the card face (bioExpanded/bioOverflows + $lib/bio-overflow.js) and shows it
	// whole in the detail pane. The TREATMENT is the precedent (lazy-fetched, cached,
	// absent-means-absent); the TRIGGER (hover on a roster row) is new here because a roster row has
	// no card face to clamp onto — the row is one line, so the bio surfaces on hover instead. A roster
	// npub need not be a contact, so the bio comes from their PUBLISHED profile via pasteKey(npub) —
	// the existing helper that resolves a profile from just an npub (chat's fetchNonContactNames uses
	// it for the same purpose); there is no second resolution path. ONE fetch per hovered person,
	// cached; a hover NEVER sweeps the whole roster. TRI-STATE (GLM review, the QURATOR-134 idiom):
	// a string is "asked and here it is", `false` is the honest "asked and there is none" (pasteKey
	// resolves a profile with no bio just as truly as a profile with one), and `'retry'` is "couldn't
	// ask — relay unreachable". A REJECTED resolve must not poison the cache: caching the failure as
	// `false` asserted the bio absent for the rest of the session, across every roster that npub
	// appears in, with no retry; the reject is recorded only as retry-later, so a later hover asks
	// again. A resolved empty string ('') is a REAL bio that happens to be empty — rendered as-is
	// under the string branch, never conflated with `false`'s stated-nothing line.
	let rosterBios: Record<string, string | false | 'retry'> = $state({});
	const fetchingBios = new Set<string>(); // in-flight guard — hover events re-fire on jitter
	let rosterHover: string | null = $state(null);

	async function rosterBioOnHover(npub: string) {
		rosterHover = npub;
		// Only a RESOLVED answer is final; a reject ('retry') asks again on the next hover.
		const cached = rosterBios[npub];
		if (cached !== undefined && cached !== 'retry') return;
		if (fetchingBios.has(npub)) return; // already asking
		fetchingBios.add(npub);
		try {
			const resolved = await pasteKey(npub);
			// The local cache answers first for a saved contact: a petname-bearing local copy beats a
			// possibly-staler relay copy, and skips the round-trip entirely.
			const contact = $contacts.find((c) => c.npub === npub);
			const bio = contact?.profile?.bio ?? resolved.profile?.bio ?? false;
			rosterBios = { ...rosterBios, [npub]: bio };
		} catch {
			rosterBios = { ...rosterBios, [npub]: 'retry' }; // couldn't ask — NOT absent; a later hover retries
		} finally {
			fetchingBios.delete(npub);
		}
	}

	// ── QURATOR-144 W2 — the self-populating sidebar ─────────────────────────────────────────
	// Merged list: local `mine` ∪ discovered, keyed on topic_id, JOINED WINS — a joined public
	// Topic renders from the local record (its name/description are what the member manages), never
	// from the announce. Private joined Topics carry no announce and can never be discovered, so
	// they simply ride the joined half. The row shape is a superset so one template serves both.
	interface TreeRow {
		topic_id: string;
		name: string;
		description: string;
		joined: boolean;
		private: boolean;
		member_count_estimate: number | null;
	}
	let mineIds = $derived(new Set(mine.map((t) => t.topic_id)));
	let mergedRows = $derived.by(() => {
		const rows: TreeRow[] = mine.map((t) => ({
			topic_id: t.topic_id,
			name: t.name,
			description: t.description,
			joined: true,
			private: t.private,
			member_count_estimate: null,
		}));
		// `directory` is already deduped by topic_id inside paintDirectory, and the joined set wins:
		// an announced topic you ARE in does not get a second row from its announce.
		for (const d of directory) {
			if (mineIds.has(d.topic_id)) continue;
			rows.push({
				topic_id: d.topic_id,
				name: d.name,
				description: d.description,
				joined: false,
				private: false,
				member_count_estimate: d.member_count_estimate,
			});
		}
		return rows;
	});
	// The path-only filter (owner ruling): matches `name` only — never the description — so every
	// match is self-evident in the row label itself. Matching groups are force-opened.
	let filteredRows = $derived.by(() => {
		const q = searchQuery.trim().toLowerCase();
		if (!q) return mergedRows;
		return mergedRows.filter((r) => r.name.toLowerCase().includes(q));
	});
	// QURATOR-147 W5: one shared root derivation (`topicRootOf`) feeds the row filter and the group
	// seed below — a name whose first segment is not a category root (e.g. the legacy rootless
	// private "back room") must land under `other` here exactly as it does in `groupTopicsByRoot`,
	// or the group header and its rows would disagree.
	function rowsForRoot(root: string): TreeRow[] {
		return filteredRows.filter((r) => topicRootOf(r.name) === root);
	}
	let groups = $derived(groupTopicsByRoot(filteredRows));

	// Collapsed groups: seeded by the default-open rule (a group is open if you are joined to
	// anything under it — a PRIVATE join opens its group too: the row is otherwise unreachable,
	// hidden behind a collapsed header for a Topic you are a member of), pure-discovery groups
	// start collapsed, user-toggled thereafter, and force-opened by a live filter. The seed runs
	// once per root as that root first appears (`seededRoots` is a plain Set, not state — a later
	// reorder or repaint must never re-collapse a group the user deliberately expanded).
	// QURATOR-149: a join that LANDS under an already-seeded root opens it via `loadMine`'s
	// transition (the `gained` un-collapse), so this set is ONLY the seed input — `isCollapsed`
	// never consults it as an override.
	let rootsWithJoined = $derived(new Set(mine.map((t) => topicRootOf(t.name))));
	let seededRoots = new Set<string>();
	$effect(() => {
		for (const r of mergedRows) {
			const root = topicRootOf(r.name);
			if (seededRoots.has(root)) continue;
			seededRoots.add(root);
			if (!rootsWithJoined.has(root)) collapsedRoots = new Set([...collapsedRoots, root]);
		}
	});
	function isCollapsed(root: string, hasMatches: boolean): boolean {
		if (searchQuery.trim() && hasMatches) return false; // the filter force-opens matching groups
		// QURATOR-149: no `rootsWithJoined` override here — the chevron must stay a live control in
		// the groups the user works in. The join transition opens its group in `loadMine` instead.
		return collapsedRoots.has(root);
	}
	function toggleGroup(root: string) {
		const next = new Set(collapsedRoots);
		if (next.has(root)) next.delete(root);
		else next.add(root);
		collapsedRoots = next;
		// Expanding reveals rows the paint-time pass deliberately did NOT rank (collapsed groups
		// were skipped so an invisible row never spent a read) — rank them now, lazily. Coalesced
		// on a short window: opening several groups in quick succession (the W1 test's two-root
		// case, or a user batch-opening) must still batch into ONE round-robin interleave, never
		// one topicRank call per group.
		scheduleExpandRank();
	}
	// The coalescing window for expand-triggered ranking (see toggleGroup). 100ms is "same burst"
	// for human double-opens and test sequences alike, and stays imperceptible next to the relay
	// round-trip it precedes.
	const EXPAND_RANK_COALESCE_MS = 100;
	function scheduleExpandRank() {
		rankGeneration += 1; // any in-flight fold is superseded by this newer pass
		const generation = rankGeneration;
		if (expandRankTimer) clearTimeout(expandRankTimer);
		expandRankTimer = setTimeout(() => {
			expandRankTimer = undefined;
			void rankDrawnRows(generation);
		}, EXPAND_RANK_COALESCE_MS);
	}

	// The per-group draw cap (r4): a group draws its ~25 most popular rows and STATES the remainder
	// ("+N more under X") — never a silent truncation (M9 posture). Joined rows are never truncated,
	// so they are pulled out first and the cap spends what is left on the unjoined tail.
	function visibleRows(rows: TreeRow[]): { drawn: TreeRow[]; remainder: number } {
		const joinedRows = rows.filter((r) => r.joined);
		const unjoined = rows.filter((r) => !r.joined);
		const kept = unjoined.slice(0, Math.max(0, TOPIC_GROUP_DRAW_CAP - joinedRows.length));
		return { drawn: [...joinedRows, ...kept], remainder: unjoined.length - kept.length };
	}

	// Selecting an unjoined row: show what a non-member can honestly be shown (blurb + claimed
	// count + Join — never the roster). The claimed count is fetched lazily HERE (never in the
	// list — r4: the sidebar orders by counts, never displays them); a rank already on file is
	// reused so this is usually a cache hit, and null must never render as "0 claimed".
	let selectedDiscovered = $derived(
		selectedDiscoveredId === null ? null : directory.find((d) => d.topic_id === selectedDiscoveredId) ?? null,
	);
	async function selectDiscovered(d: DiscoveredTopic) {
		selectedDiscoveredId = d.topic_id;
		openTopic = null;
		selectedClaimed = null;
		if (d.member_count_estimate !== null) {
			selectedClaimed = d.member_count_estimate;
			return;
		}
		try {
			const ranks = await topicRank([{ topic_id: d.topic_id, name: d.name }]);
			// A stale resolve (the user moved on to another row) must not bind here.
			if (selectedDiscoveredId === d.topic_id && ranks.length > 0) {
				selectedClaimed = ranks[0].member_count_estimate;
				// QURATOR-148: the same answer carries aliveness. A KNOWN dead Topic (no member
				// pinged in 30 days) must not keep a selected row alive either — drop the selection
				// so the row leaves the directory and the detail pane returns to the empty state.
				if (ranks[0].alive_count === 0) {
					directory = directory.filter((x) => x.topic_id !== d.topic_id);
					deadDropped += 1;
					if (selectedDiscoveredId === d.topic_id) selectedDiscoveredId = null;
					selectedClaimed = null;
				}
			}
		} catch {
			/* the count is cosmetic — the row's honest content does not depend on it */
		}
	}
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
		<button class="btn-primary" onclick={() => (createOpen = true)}>+ New Topic</button>
	</div>
</div>

<div class="body">
	<section class="master-detail">
		<!-- Left: the merged directory tree (QURATOR-144 W2). Joined rows in white with the admin
		     meta line; unjoined rows muted with their blurb + Join. Colour is never the only tell —
		     the two states differ STRUCTURALLY (meta line + unread pill vs blurb + Join). -->
		<div class="list-pane">
			<!-- QURATOR-144 W2: path-only filter over the whole merged tree; matching groups are
			     force-opened by `isCollapsed`. -->
			<div class="discover-search">
				<input
					class="hb-input"
					type="search"
					placeholder="Filter by path…"
					bind:value={searchQuery}
					aria-label="Filter Topics by path"
				/>
			</div>
			{#if mineLoadError && mergedRows.length === 0}
				<!-- QURATOR-93: a FAILED loadMine is not the confident "You haven't joined any Topics
				     yet" negative. QURATOR-152/154: the whole-pane error is only for a failure with
				     NOTHING painted — see the inline notice in the tree branch below. -->
				<EmptyState
					error
					message="Couldn't load your Topics."
					onretry={loadMine}
				/>
			{:else if paintError && mergedRows.length === 0}
				<!-- QURATOR-80, one-tree form: a failed paint is retryable, never the confident
				     negative. -->
				<EmptyState
					error
					message="Couldn’t reach the relays."
					onretry={() => { painted = false; paintError = false; void paintDirectory(); }}
				/>
			{:else if mergedRows.length === 0}
				<!-- QURATOR-148: an empty tree is not always "nothing exists" — a paint can return
				     rows whose Topics are all dead (no member pinged in 30 days), which the rank
				     fold drops. Painted-but-dropped is a DIFFERENT honest state from never-painted:
				     it says the directory was read and nothing in it is worth joining. -->
				{#if painted && directory.length === 0 && deadDropped > 0}
					<EmptyState message="No live Topics right now — every public Topic in the directory went quiet over 30 days ago. Create one, or check back later." />
				{:else}
					<EmptyState message="Nothing here yet. Create a Topic, or join one from the directory." />
				{/if}
			{:else}
				{#each groups as g (g.root)}
					{@const rows = rowsForRoot(g.root)}
					{@const hasMatches = rows.length > 0}
					{#if hasMatches || !searchQuery.trim()}
						<div class="root-group">
							<button class="root-header" onclick={() => toggleGroup(g.root)} aria-expanded={!isCollapsed(g.root, hasMatches)}>
								<span class="root-chevron" class:open={!isCollapsed(g.root, hasMatches)}>{@html icons.chevronRight}</span>
								<span class="root-name">{g.root}</span>
								<span class="root-count muted">{rows.length}</span>
							</button>
							{#if !isCollapsed(g.root, hasMatches)}
								{@const vis = visibleRows(rows)}
								{#each vis.drawn as r (r.topic_id)}
									{#if r.joined}
										<!-- Joined: white, with the admin meta line (roster + channel live in the
										     detail pane). Never truncated, never muted. -->
										<button class="topic-row" class:topic-selected={openTopic?.topic_id === r.topic_id} onclick={() => open(mine.find((t) => t.topic_id === r.topic_id)!)}>
											<div class="grow">
												<div class="name">{r.name} {#if r.private}<span class="hb-tag">private</span>{/if}</div>
												{#if r.description}<div class="muted">{r.description}</div>{/if}
												<div class="muted row-meta">joined</div>
											</div>
											<!-- Unread pill (draft r1): this Topic's announcement is past its seen
											     watermark. Mirrors the Chat nav-badge's visual language. -->
											{#if unseenTopics.has(r.topic_id)}
												<span class="unread" title="New announcement"></span>
											{/if}
										</button>
									{:else}
										<!-- Unjoined: the announce's own blurb + a Join affordance. `--fg-muted`,
										     NOT `--fg-dim` — dim fails contrast on --bg-raised and a whole
										     directory under 4.5:1 is a defect, not a state. Never disabled. -->
										<div class="row tree-child unjoined" role="button" tabindex="0" onclick={() => selectDiscovered(directory.find((d) => d.topic_id === r.topic_id)!)} onkeydown={(e) => e.key === 'Enter' && selectDiscovered(directory.find((d) => d.topic_id === r.topic_id)!)}>
											<div class="grow">
												<div class="name">{subPathLabel(r.name) || r.name}</div>
												{#if r.description}<div class="blurb">{r.description}</div>{/if}
											</div>
											<!-- The Join button stops propagation: without it the click also bubbles into
											     the row's own click-to-select handler, firing an unwanted extra topicRank
											     call for a Topic the user is about to join. -->
											<button class="btn-default" onclick={(e) => { e.stopPropagation(); askToJoin(r.name, false); }}>Join</button>
										</div>
									{/if}
								{/each}
								{#if vis.remainder > 0}
									<!-- r4 / M9 posture: the cap states its remainder, never a silent
									     truncation. Joined rows are never truncated (visibleRows). -->
									<div class="root-status muted">+{vis.remainder} more under {g.root}</div>
								{/if}
							{/if}
						</div>
					{/if}
				{/each}
				{#if groups.length === 0}
					<!-- A filter that matches nothing: an honest empty, not an error. -->
					<EmptyState message="No Topics match that path." />
				{/if}
				<!-- GLM review: a failed paint with JOINED rows on screen used to show NOTHING — the
				     tree silently read as "no public Topics exist" (the exact QURATOR-80 confusion
				     this redesign exists to avoid). Keep the still-valid joined rows visible; the
				     failure rides BELOW them in the same retryable error dialect, never as a blank. -->
				{#if paintError && mergedRows.length > 0}
					<EmptyState
						error
						message="Couldn’t reach the relays — the directory may be stale."
						onretry={() => { painted = false; paintError = false; void paintDirectory(); }}
					/>
				{/if}
				<!-- QURATOR-152/154 (W2 merge + W3 instant paint): a failed LOCAL `topicList` read is the
				     "joined Topics" half's failure — it must not replace a directory that just painted
				     (from cache or fresh). It rides BELOW the tree in the same retryable error dialect
				     the paintError banner above uses, never as a blank pane. -->
				{#if mineLoadError}
					<EmptyState
						error
						message="Couldn't load your Topics."
						onretry={loadMine}
					/>
				{/if}
			{/if}
			<button class="link" disabled={busy} onclick={redeemInvite}>Redeem a private Topic invite</button>
		</div>

			<!-- Right: detail (roster + invite + chat deep-link for a joined Topic; the honest
			     non-member view — blurb + claimed count + Join — for an unjoined one. A grey row is
			     never disabled: selecting it shows what a non-member can honestly be shown.) -->
			<div class="detail-pane">
				{#if openTopic}
					<div class="detail-head">
						<div class="grow">
							<div class="detail-title">{openTopic.name} {#if openTopic.private}<span class="hb-tag">private</span>{/if}</div>
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
						<!-- QURATOR-146: each row is a real <button> (keyboard-reachable; Enter fires the
						     same navigation as double-click) wrapped in the hover region. The bio rides the
						     same hover as the chat hand-off so the row explains itself once. -->
						<ul class="roster">
							{#each roster as npub (npub)}
								{@const row = rosterRowProps(npub)}
								{@const bio = rosterHover === npub ? rosterBios[npub] : undefined}
								<li class="roster-item">
									{#if row.isSelf}
										<!-- The self row keeps r1's plain non-interactive form — there is no "talk to
										     yourself" hand-off to offer. -->
										<div class="roster-row self">
											<PersonRow name={row.name} letter={row.letter} picture={row.picture} fingerprint={row.fingerprint} online={row.online} />
										</div>
									{:else}
										<button
											type="button"
											class="roster-row"
											title="Double-click or press Enter to open a chat with this member"
											onfocus={() => rosterBioOnHover(npub)}
											onblur={() => (rosterHover = rosterHover === npub ? null : rosterHover)}
											onmouseenter={() => rosterBioOnHover(npub)}
											onmouseleave={() => (rosterHover = rosterHover === npub ? null : rosterHover)}
											ondblclick={() => openRosterChat(npub)}
											onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); openRosterChat(npub); } }}
										>
											<PersonRow name={row.name} letter={row.letter} picture={row.picture} fingerprint={row.fingerprint} online={row.online} />
											<span class="roster-cue" aria-hidden="true">chat ⏎</span>
										</button>
										{#if bio !== undefined && bio !== 'retry'}
											<div class="roster-bio" role="tooltip">
												<!-- Absent-means-absent: a resolved "no bio" is a stated nothing, never a
												     blank card — the same honesty rule as PersonRow's omitted fingerprint.
												     'retry' (a rejected resolve) renders NOTHING here — it is "couldn't
												     ask", not "asked and there is none", and the next hover asks again. -->
												{bio === false ? 'No published profile' : bio}
											</div>
										{/if}
									{/if}
								</li>
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
								placeholder="Pushes a highlighted notice to all members' channel view for 24h. Limited to one per hour."
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
						<!-- Owner, 2026-08-27: the terms moved INTO the placeholder and the label underneath was
						     deleted. Same facts, one surface instead of two — the field now states its own terms
						     where you are about to type, rather than restating them below the button. The
						     HintMarker keeps the long-form ANNOUNCE_EXPLAINER for the "?" affordance, and
						     announce-view.test.ts still pins that constant. -->
					</div>

					<a class="channel-link" href="/chat?topic={openTopic.topic_id}">💬 Open this Topic’s channel in Chat →</a>
				{:else if selectedDiscovered}
					<!-- W2: the non-member detail. The claimed count is fetched lazily on selection and
					     renders ONLY here (r4: the sidebar orders by counts, never displays them); null
					     is "unknown", never "0 claimed". -->
					<div class="detail-head">
						<div class="grow">
							<div class="detail-title">{selectedDiscovered.name}</div>
							{#if selectedDiscovered.description}
								<div class="desc-row"><span class="muted">{selectedDiscovered.description}</span></div>
							{:else}
								<div class="desc-row"><span class="muted desc-empty">No description</span></div>
							{/if}
						</div>
						<button class="btn-primary" disabled={busy} onclick={() => askToJoin(selectedDiscovered!.name, false)}>Join</button>
					</div>
					<div class="detail-section">
						<div class="section-label">Members</div>
						<div class="muted">
							{#if selectedClaimed === null}
								unknown
							{:else}
								{memberCountLabel(selectedClaimed)}
							{/if}
						</div>
					</div>
					<div class="muted">You’re not a member of this Topic yet. Joining is always gated by an explicit confirmation.</div>
				{:else}
					<div class="detail-empty">Select a Topic to see its roster, invite members, and open its chat channel.</div>
				{/if}
			</div>
		</section>
</div>

<!-- Create-a-Topic modal (devtest #9: was an always-on card; now invoked from "+ New Topic"). -->
<Modal open={createOpen} title="New Topic" closeOnBackdrop={false} onclose={() => (createOpen = false)}>
	<div class="create-fields">
		<!-- W4: a Topic is a category root (picker) + freeform sub-path. The root picker makes a
		     non-category root unrepresentable; the backend re-validates authoritatively. QURATOR-147
		     W5: the SAME picker serves private Topics — private obeys the public path convention, so
		     the convention is the path of least resistance, not folklore. -->
		<div class="path-row">
			<select class="hb-input" bind:value={newRoot}>
				{#each TOPIC_ROOTS as r}<option value={r}>{r}</option>{/each}
			</select>
			<span class="path-sep">/</span>
			<input class="hb-input grow" placeholder="optional sub-path (e.g. animation/anime)" bind:value={newSubPath} />
		</div>
		<div class="muted path-preview">Topic path: <code>{composedName}</code></div>
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

	/* QURATOR-144 W2 — the directory tree in the list pane. Root groups collapse per the
	   default-open rule (joined-under-it = open; pure discovery = collapsed). */
	.discover-search { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
	.discover-search input { flex: 1; }
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
	.root-count { font-size: 10.5px; font-weight: 400; margin-left: auto; }
	.root-status { padding: 6px 0 6px 22px; }
	/* The joined row's admin meta line — a member's marker, structural (not colour-only): it is
	   what makes the two row states differ for a screen reader and in greyscale. */
	.row-meta { margin-top: 1px; }
	/* Unjoined rows: --fg-muted, NOT --fg-dim — dim fails contrast on --bg-raised, and a whole
	   directory under 4.5:1 is a defect, not a state. The row stays interactive (never disabled). */
	.unjoined { color: var(--fg-muted); cursor: pointer; }
	.unjoined:hover { background: var(--bg-elev2); border-radius: 7px; color: var(--fg); }
	.unjoined .blurb { font-size: 11.5px; color: var(--fg-muted); }

	/* Shared controls — M20 W4: inputs use the global .hb-input contract (app.css); the bare
	   `input {}` element selector is gone (it filled --bg-elev2 and leaked onto modal fields). */
	/* M15 W1: buttons unified on the app.css .btn system. `.link` stays a local text-link (no boxed
	   equivalent in the shared system); `button:disabled` keeps the .link dim state. */
	button:disabled { opacity: 0.5; cursor: not-allowed; }
	button.link { background: transparent; border: none; color: var(--accent); text-align: left; padding: 4px 0; margin-top: 4px; cursor: pointer; }
	.check { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--fg-muted); }
	.grow { flex: 1; min-width: 0; }
	.row { display: flex; align-items: center; gap: 8px; padding: 6px 0; border-top: 1px solid var(--divider); }
	.name { font-size: 13px; font-weight: 600; }
	.muted { font-size: 11.5px; color: var(--fg-dim); }
	.path-row { display: flex; align-items: center; gap: 6px; }
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
	/* QURATOR-146 — the row becomes the hover region AND the focusable hand-off. The button fills the
	   row so the whole strip is clickable/keyboard-focusable; the cue appears only while the row is
	   hovered or focused (the affordance a bare double-click gesture would never advertise). */
	.roster-item { padding: 0; position: relative; }
	.roster-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		width: 100%;
		padding: 3px 4px;
		background: transparent;
		border: none;
		border-radius: 5px;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: default;
	}
	.roster-row:hover, .roster-row:focus-visible { background: var(--bg-elev2); cursor: pointer; }
	.roster-row:focus { outline: none; } /* :focus-visible carries the ring; a pointer click stays quiet */
	.roster-row.self { cursor: default; } /* no hand-off for the self row — nothing to talk to */
	.roster-cue { visibility: hidden; font-size: 10.5px; color: var(--fg-dim); white-space: nowrap; }
	.roster-row:hover .roster-cue, .roster-row:focus-visible .roster-cue { visibility: visible; }
	/* The bio surfaces under the row on hover/focus — inline expansion, NOT a floating hovercard
	   (Contacts' precedent is an inline clamp + detail pane; a floating card is a new pattern this
	   page deliberately does not introduce). max-height 0 → auto is a transition nicety only: jsdom
	   computes no layout, so nothing here is visually proven by tests. */
	.roster-bio {
		margin: 2px 4px 4px 36px; /* aligns under the name, past the 24px avatar + 8px gap */
		padding: 4px 8px;
		font-size: 11.5px;
		line-height: 1.45;
		color: var(--fg-muted);
		background: var(--bg-elev1);
		border-left: 2px solid var(--accent);
		border-radius: 4px;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.invite { display: flex; gap: 6px; }
	.announce-row { display: flex; gap: 6px; margin-top: 2px; }
	.announce-row input { flex: 1; }
	/* Hoardbook Topics draft r1 — unread pill: the per-row announce share, on the nav-badge's accent
	   fill (boolean only — this data shape carries no per-topic count). */
	.unread {
		width: 8px; height: 8px; border-radius: 999px; flex-shrink: 0;
		background: var(--accent); box-shadow: 0 0 0 1px var(--bg-elev1);
	}
	.channel-link { display: inline-block; margin-top: 4px; font-size: 12px; color: var(--accent); text-decoration: none; }
	.channel-link:hover { text-decoration: underline; }
	/* M15 W2: the two Topic modals now use Modal.svelte; only the create form's field spacing is local. */
	.create-fields { display: flex; flex-direction: column; gap: 8px; }
</style>
