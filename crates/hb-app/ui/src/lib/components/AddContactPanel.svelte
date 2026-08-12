<script lang="ts">
	// "+ Add contact" modal (devtest #17/#18 phonebook redesign) — the lookup-by-ID card + §6 Discover
	// section, MOVED here verbatim from contacts/+page.svelte (byte-for-byte row/card markup) so both
	// existing add entry points keep working unchanged: a lookup-card "Add contact" and a Discover-hit
	// "Add contact" both call `onadd`, which the page routes into its existing `openAddContact` →
	// AddContactDialog → completeFollow funnel (petname + group picker, then `follow`).
	import { pasteKey, searchPeers, discoverObservedTags, type PeerSearchHit, type PeerSearchResult } from '../api.js';
	import { contacts, identity, toast } from '../stores.js';
	import { icons, avatarHue } from '../icons.js';
	import Avatar from './Avatar.svelte';
	import FeatureTooltip from './FeatureTooltip.svelte';
	import Modal from './Modal.svelte';
	import type { CachedPeer } from '../types.js';
	import { renderFingerprint } from '../identity-display.js';
	import { DISCOVER_CONTENT_TYPES, parseTagInput, canSearch, toggleContentType, DISCOVER_PAGE_SIZE, pageItems, pageCount, suggestTags } from '../discover-view.js';

	interface Props {
		open?: boolean;
		// `code` is what `follow` must re-resolve — the full `hbk1…` share code (carrying the
		// browse-key) for a lookup, or the bare npub for a discovery hit. Passing only the npub
		// (as before) silently dropped the key and made every added contact keyless (devtest #3).
		// M20 W2: `resolved` is the CachedPeer the lookup already produced — carried through so the
		// follow leg skips a SECOND resolve. A discovery hit carries `null` (no pre-resolved peer).
		onadd?: (code: string, npub: string, displayName: string, resolved: CachedPeer | null) => void;
		// M17 W1: "Message" on a discovery hit-card → `/chat?compose=<npub>` (works for non-contacts).
		// "Add contact" stays primary/first; Message comes after it.
		onmessage?: (npub: string) => void;
		onclose?: () => void;
	}

	let { open = false, onadd, onmessage, onclose }: Props = $props();

	// Lookup state
	let input = $state('');
	let loading = $state(false);
	let result = $state<CachedPeer | null>(null);
	// The exact string that produced `result` — threaded to `follow` so the browse-key survives the
	// add (devtest #3). Captured at lookup time so a later edit to `input` can't desync it.
	let lookedUpCode = $state('');

	let existingContact = $derived($contacts.find((c) => c.npub === result?.npub));
	let alreadyFollowed = $derived(!!existingContact);
	// devtest #4: a contact added by npub/discovery is keyless. Pasting their FULL share code later
	// must be allowed to attach the browse-key (re-adding overwrites the stored contact) — otherwise
	// the "Added"/disabled button dead-ends the upgrade and they stay permanently unbrowseable.
	let canUnlock = $derived(!!result?.browse_key_hex && !!existingContact && !existingContact.browse_key_hex);

	async function handleLookup() {
		const id = input.trim();
		if (!id) return;
		// devtest #14 self-guard — the same exact-match check the page used to run inline.
		if (id === $identity?.npub) {
			toast("That's your own ID — you can't add yourself as a contact.", 'error');
			return;
		}
		loading = true;
		result = null;
		try {
			result = await pasteKey(id);
			lookedUpCode = id;
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			loading = false;
		}
	}

	function handleFollow() {
		if (!result) return;
		// M20 W2: carry the lookup's resolved peer through to the follow leg so it isn't resolved twice.
		onadd?.(lookedUpCode, result.npub, result.profile?.display_name ?? '', result);
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') handleLookup();
	}

	// ── §6 Discovery (moved from Browse — devtest 2026-06-25 #6) ─────────────────────────────────
	let discoverOpen = $state(false);
	let discoverTags = $state('');
	let discoverTypes: string[] = $state([]);
	let discoverResults: PeerSearchHit[] = $state([]);
	let discoverCapped = $state(false); // M20 W3: more candidates existed than the cap kept
	let discovering = $state(false);
	let discoverError = $state('');
	let discovered = $state(false); // a search has run at least once (drives the empty-vs-no-results copy)
	// QURATOR-44: pagination replaces the old "Showing first N — narrow your search" affordance.
	let discoverPage = $state(1);
	let parsedDiscoverTags = $derived(parseTagInput(discoverTags));
	let canDiscover = $derived(canSearch(parsedDiscoverTags, discoverTypes));
	// QURATOR-44: only DISCOVER_PAGE_SIZE (10) hits show per page; the ranked set is stable per
	// search (the Rust ranker derives a deterministic seed from the filter terms), so pages never
	// dup or skip.
	let discoverPageItems = $derived(pageItems(discoverResults, discoverPage));
	let discoverPageCount = $derived(pageCount(discoverResults.length));

	// QURATOR-70 — tag autocomplete. Multi-term search is strict AND-on-tags (the contract DISC1/WAN-D
	// pin); single-term widens to fuzzy name/bio. The chip affordance is LOAD-BEARING for that rule:
	// it makes the second term a real observed tag picked from a list, rather than hopeful free text
	// that silently switches search kind (fuzzy → strict AND) and makes a hit vanish.
	let observedTags: string[] = $state([]);
	let tagsLoaded = $state(false);

	async function loadObservedTags() {
		if (tagsLoaded) return;
		try {
			observedTags = await discoverObservedTags();
			tagsLoaded = true;
		} catch {
			// Local-cache read; failures are non-fatal — autocomplete just stays empty.
			tagsLoaded = true;
		}
	}

	/** The last comma/space-separated token the user is currently typing (the partial stem the
	 *  autocomplete filters on). This is the tail of the input after the last separator. */
	let typedStem = $derived.by(() => {
		const raw = discoverTags;
		const lastSep = Math.max(raw.lastIndexOf(','), raw.lastIndexOf(' '));
		return raw.slice(lastSep + 1).trim();
	});

	/** The COMMITTED tags: every parsed tag EXCEPT the one currently being typed (the tail stem).
	 *  These are the chips already chosen; the autocomplete excludes them so a committed tag is never
	 *  re-offered. The stem itself is NOT excluded — typing 'vhs' must still offer 'vhs'. */
	let committedTags = $derived.by(() => {
		const stem = typedStem;
		return parsedDiscoverTags.filter((t) => t !== stem);
	});

	/** Tags from the observed set that the user has NOT yet committed, filtered by the current typed
	 *  stem. Shown only when the stem is non-empty. Uses the shared pure `suggestTags` helper so the
	 *  filter math is unit-tested in discover-view.test.ts. */
	let autocompleteSuggestions = $derived(
		typedStem.length > 0
			? suggestTags(observedTags, committedTags, typedStem)
			: []
	);

	function addTagChip(tag: string) {
		// Append the chosen tag to the committed tag set, then clear the free-text input so the next
		// pick starts fresh. The chips render the committed set (parsedDiscoverTags derives from
		// discoverTags); clearing the input keeps the autocomplete from re-offering the just-chosen tag
		// and makes the "pick another" flow obvious.
		const normalized = tag.trim().toLowerCase();
		if (!normalized || parsedDiscoverTags.includes(normalized)) {
			discoverTags = '';
			return;
		}
		discoverTags = [...parsedDiscoverTags, normalized].join(', ');
		// Clear the typed stem by resetting the input to the committed set (chips render from this).
		// The chips + this string are the same model: the committed tags, comma-separated.
	}

	function removeTagChip(tag: string) {
		const remaining = parsedDiscoverTags.filter((t) => t !== tag);
		discoverTags = remaining.join(', ');
	}

	async function runDiscover() {
		if (!canDiscover) { discoverError = 'Enter at least one tag or content type to search.'; return; }
		discovering = true;
		discoverError = '';
		try {
			const result: PeerSearchResult = await searchPeers(parsedDiscoverTags, discoverTypes);
			discoverResults = result.hits;
			discoverCapped = result.capped;
			discoverPage = 1; // QURATOR-44: reset to page 1 on each new search.
			discovered = true;
		} catch (e) {
			discoverError = String(e);
		} finally {
			discovering = false;
		}
	}

	function toggleDiscoverOpen() {
		discoverOpen = !discoverOpen;
		// QURATOR-70: load observed tags the first time the Discover section opens so the chip
		// autocomplete is ready before the user starts typing.
		if (discoverOpen) loadObservedTags();
	}

	function followHit(hit: PeerSearchHit) {
		// bare npub only: awareness, NOT a browse-key (INV-2) — the dialog's Skip path preserves that.
		// Discovery hits are teaser-only (DISC3) — no browse-key exists, so the code IS the npub.
		// M20 W2: null resolved peer — a discovery hit was never resolved, so follow resolves it.
		onadd?.(hit.npub, hit.npub, hit.display_name, null);
	}

	function close() {
		onclose?.();
	}
</script>

<Modal open={open} width="520px" padding="0" onclose={close}>
	<div class="acp-frame">
		<div class="acp-modal-head">
			<h2>Add contact</h2>
			<button type="button" class="acp-close" aria-label="Close" onclick={close}>{@html icons.close}</button>
		</div>
		<div class="acp-body">
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
								onkeydown={handleKeydown}
							/>
						</div>
						<button class="btn-primary" onclick={handleLookup} disabled={!input.trim() || loading}>
							{loading ? 'Looking up…' : 'Lookup'}
						</button>
					</div>

					{#if result}
						<div class="result">
							<div class="profile-card">
								<div class="profile-banner"></div>
								<div class="profile-inner">
									<div class="profile-top">
										<Avatar
											letter={(result.profile?.display_name || result.npub)[0].toUpperCase()}
											size={52}
											hue={avatarHue((result.profile?.display_name || result.npub)[0])}
											picture={result.profile?.picture}
										/>
										<div class="profile-name-col">
											<div class="name-row">
												<span class="peer-name">{result.profile?.display_name || 'Unknown'}</span>
												{#if result.online}
													<span class="pill pill-online"><span class="pill-dot"></span> Online</span>
												{:else}
													<span class="pill pill-offline">Offline</span>
												{/if}
											</div>
											<span class="mono">{result.npub.slice(0, 18)}…{result.npub.slice(-4)}</span>
										</div>
										<div class="profile-actions">
											<button
												class="btn-primary btn-sm"
												onclick={handleFollow}
												disabled={alreadyFollowed && !canUnlock}
											>
												{canUnlock ? 'Unlock browsing' : alreadyFollowed ? 'Added' : 'Add contact'}
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
											<span class="fp-swatch" style="background:{result.fingerprint.colorHex}"></span>
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
				     clutter the panel; results are the opt-in public teaser only (listings stay 🔒 locked). -->
				<div class="discover-section">
					<button class="discover-toggle" onclick={toggleDiscoverOpen} aria-expanded={discoverOpen}>
						<span class="discover-toggle-label">{@html icons.search} Discover hoarders</span>
						<span class="discover-chevron" class:open={discoverOpen}>{@html icons.chevronDown}</span>
					</button>
					{#if discoverOpen}
						<div class="discover-body">
							<div class="discover-sub">Search public profiles by name, bio, or tag. One term searches broadly; two or more tags narrow (pick tags from the list as you type). Only what people chose to announce is searchable — everyone's listings stay encrypted.</div>
							<div class="ct-row">
								{#each DISCOVER_CONTENT_TYPES as ct (ct.value)}
									<button type="button" class="ct-chip" class:ct-on={discoverTypes.includes(ct.value)}
										onclick={() => (discoverTypes = toggleContentType(discoverTypes, ct.value))}>{ct.label}</button>
								{/each}
							</div>
							<form class="disc-tag-row" onsubmit={(e) => { e.preventDefault(); runDiscover(); }}>
								<div class="disc-tag-input-wrap">
									{#if committedTags.length > 0}
										<div class="disc-chip-row">
											{#each committedTags as tag (tag)}
												<span class="disc-tag-chip">
													#{tag}
													<button type="button" class="disc-chip-x" aria-label="Remove tag {tag}" onclick={() => removeTagChip(tag)}>{@html icons.close}</button>
												</span>
											{/each}
										</div>
									{/if}
									<input
										class="hb-input disc-tag-input"
										placeholder="name, bio, or tag (e.g. anime, vhs)"
										bind:value={discoverTags}
										onfocus={() => loadObservedTags()}
									/>
									{#if autocompleteSuggestions.length > 0}
										<!-- QURATOR-70: tag autocomplete — picking a real observed tag makes multi-term
										     narrowing tag-driven by construction (the contract strict-AND survives, and the
										     user understands the second term is a tag, not hopeful free text). -->
										<ul class="disc-autocomplete" role="listbox" aria-label="Suggested tags">
											{#each autocompleteSuggestions as tag (tag)}
												<li role="option" aria-selected="false">
													<button type="button" class="disc-ac-item" onmousedown={(e) => e.preventDefault()} onclick={() => addTagChip(tag)}>
														<span class="disc-ac-hash">#</span>{tag}
													</button>
												</li>
											{/each}
										</ul>
									{/if}
								</div>
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
									{#each discoverPageItems as hit (hit.npub)}
										{@const letter = (hit.display_name?.[0] ?? hit.npub[0]).toUpperCase()}
										<div class="hit-card">
											<div class="hit-top">
												<Avatar {letter} size={30} hue={avatarHue(letter)} picture={hit.picture ?? undefined} />
												<div class="hit-id">
													<span class="hit-name">{hit.display_name || hit.npub.slice(0, 12) + '…'}</span>
													<span class="hit-stranger" title="Verify the fingerprint before trusting a stranger">unverified — not in your contacts</span>
												</div>
												<button class="hit-follow" onclick={() => followHit(hit)}>Add contact</button>
												<button class="hit-message" onclick={() => onmessage?.(hit.npub)}>Message</button>
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
								{#if discoverPageCount > 1}
									<!-- QURATOR-44: pagination replaces the old "Showing first N — narrow your search"
									     (hoarder #101). Page size 10; the ranked set is stable per search so pages partition
									     without dupes or skips. -->
									<div class="discover-pager">
										<button type="button" class="pager-btn" disabled={discoverPage <= 1} onclick={() => (discoverPage = Math.max(1, discoverPage - 1))}>Prev</button>
										<span class="pager-info">Page {discoverPage} of {discoverPageCount}</span>
										<button type="button" class="pager-btn" disabled={discoverPage >= discoverPageCount} onclick={() => (discoverPage = Math.min(discoverPageCount, discoverPage + 1))}>Next</button>
									</div>
								{/if}
								{#if discoverCapped}
									<!-- M20 W3: the cap truncated the ranked set (more existed than SEARCH_CAP kept). Shown
									     below the pager so the user knows there may be more matches beyond the cap. -->
									<div class="discover-capped" role="status">
										More matches exist — refine your search to narrow the results.
									</div>
								{/if}
							{:else}
								<div class="discover-empty">Pick a content type or enter a tag, then Search.</div>
							{/if}
						</div>
					{/if}
				</div>
			</div>
		</div>
</Modal>
<!-- /M15 W2: AddContactPanel now wraps its head+body in Modal.svelte -->

<style>
	/* M15 W2: backdrop/card now come from Modal.svelte (base level; the petname + New-group dialogs
	   are `stacked`, so they still sit above this panel). This frame just lays out head + body. */
	.acp-frame {
		display: flex; flex-direction: column;
		max-height: min(680px, calc(100vh - 60px));
	}
	.acp-modal-head {
		display: flex; align-items: center; justify-content: space-between;
		padding: 16px 18px; border-bottom: 1px solid var(--border);
		flex-shrink: 0;
	}
	.acp-modal-head h2 { font-size: 15px; font-weight: 600; margin: 0; }
	.acp-close {
		background: transparent; border: none; cursor: pointer;
		color: var(--fg-muted); display: flex; padding: 4px;
	}
	.acp-close:hover { color: var(--fg); }
	.acp-body { padding: 18px; overflow-y: auto; display: flex; flex-direction: column; gap: 16px; }

	/* Lookup */
	.lookup-section { display: flex; flex-direction: column; }

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
	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */

	/* ── §6 Discover hoarders ───────────────────────────────────────────────────────────────── */
	.discover-section {
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
	/* QURATOR-70: the input wraps with the chip row + autocomplete dropdown so the whole control
	   stays one unit in the discover row. */
	.disc-tag-input-wrap { flex: 1; position: relative; display: flex; flex-direction: column; gap: 5px; }
	.disc-chip-row { display: flex; flex-wrap: wrap; gap: 4px; }
	.disc-tag-chip {
		display: inline-flex; align-items: center; gap: 4px;
		font-size: 10.5px; padding: 2px 6px 2px 7px; border-radius: 999px;
		color: var(--accent);
		background: color-mix(in oklch, var(--accent) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--accent) 30%, transparent);
	}
	.disc-chip-x {
		background: transparent; border: none; cursor: pointer; color: var(--fg-muted);
		display: inline-flex; padding: 0; line-height: 1;
	}
	.disc-chip-x:hover { color: var(--fg); }
	.disc-autocomplete {
		position: absolute; top: 100%; left: 0; right: 0; z-index: 10;
		margin: 2px 0 0; padding: 3px; list-style: none;
		background: var(--bg-elev2); border: 1px solid var(--border); border-radius: 7px;
		box-shadow: 0 4px 14px rgba(0,0,0,0.25);
		max-height: 220px; overflow-y: auto;
	}
	.disc-ac-item {
		display: flex; align-items: center; width: 100%; text-align: left;
		padding: 5px 8px; border: none; background: transparent; cursor: pointer;
		font-size: 12px; color: var(--fg); border-radius: 5px; font-family: var(--font-ui);
	}
	.disc-ac-item:hover { background: var(--bg-elev3); }
	.disc-ac-hash { color: var(--accent); margin-right: 2px; font-weight: 600; }
	/* M20 W4: the tag input composes the global .hb-input contract; only the flex-grow layout
	   stays local (the contract is flex:auto so this row input grows to fill the discover row). */
	.disc-tag-input { flex: 1; }
	.discover-error { font-size: 11.5px; color: oklch(0.75 0.15 25); }
	.discover-results { display: grid; grid-template-columns: repeat(auto-fill, minmax(232px, 1fr)); gap: 12px; }
	.discover-capped { text-align: center; color: var(--fg-dim); font-size: 11.5px; padding: 10px 0 2px; }
	/* QURATOR-44: pagination control (page size 10, replaces the old "showing first N" line). */
	.discover-pager {
		display: flex; align-items: center; justify-content: center; gap: 10px;
		padding: 10px 0 2px;
	}
	.pager-btn {
		padding: 3px 12px; border-radius: 6px; font-size: 11.5px; font-weight: 600;
		background: var(--bg-elev2); color: var(--fg-muted);
		border: 1px solid var(--border); cursor: pointer; font-family: var(--font-ui);
	}
	.pager-btn:hover:not(:disabled) { background: var(--bg-elev3); color: var(--fg); }
	.pager-btn:disabled { opacity: 0.4; cursor: default; }
	.pager-info { font-size: 11px; color: var(--fg-dim); font-feature-settings: 'tnum'; }
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
	/* M17 W1: secondary Message action — Add contact stays primary, this comes after it. */
	.hit-message {
		padding: 4px 12px; border-radius: 6px; background: transparent; color: var(--fg-muted);
		border: 1px solid var(--border); font-size: 11.5px; font-weight: 600; cursor: pointer; font-family: var(--font-ui); flex-shrink: 0;
	}
	.hit-message:hover { background: var(--bg-elev3); color: var(--fg); }
	.hit-bio { font-size: 11.5px; color: var(--fg-muted); overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
	.hit-fp { display: flex; align-items: center; gap: 6px; font-size: 10px; color: var(--fg-dim); font-family: var(--font-mono); }
	.hit-fp-swatch { width: 10px; height: 10px; border-radius: 3px; flex-shrink: 0; }
	.hit-tags { display: flex; flex-wrap: wrap; gap: 4px; }
	.hit-tag { font-size: 9.5px; padding: 1px 5px; border-radius: 999px; background: var(--bg-elev3); color: var(--fg-muted); border: 1px solid var(--border); }
	.hit-tag-ct { background: var(--accent-soft); color: var(--accent); border-color: color-mix(in oklch, var(--accent) 30%, transparent); }
	.hit-locked { display: inline-flex; align-items: center; font-size: 11px; color: var(--fg-dim); margin-top: 2px; }
</style>
