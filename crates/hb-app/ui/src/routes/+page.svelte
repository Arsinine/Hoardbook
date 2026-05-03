<script lang="ts">
	import { saveProfile, publishProfile, publishCollection, deleteCollection, updateCollectionMeta, getShareSettings, generateKeypair, saveSettings, hasPublishedProfile } from '$lib/api.js';
	import { profile, collections, identity, toast, appReady, homeDraft } from '$lib/stores.js';
	import { onMount } from 'svelte';
	import { icons, socialIcons, avatarHue } from '$lib/icons.js';
	import CollectionPanel from '$lib/components/CollectionPanel.svelte';
	import ScanDialog from '$lib/components/ScanDialog.svelte';
	import ShareSettingsDialog from '$lib/components/ShareSettingsDialog.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import type { Collection, Profile, SocialLink } from '$lib/types.js';

	const LANGUAGES = [
		'Afrikaans','Albanian','Arabic','Armenian','Azerbaijani','Basque','Belarusian',
		'Bengali','Bulgarian','Catalan','Chinese','Croatian','Czech','Danish','Dutch',
		'English','Estonian','Finnish','French','Galician','Georgian','German','Greek',
		'Hebrew','Hindi','Hungarian','Icelandic','Indonesian','Italian','Japanese',
		'Kannada','Kazakh','Korean','Latvian','Lithuanian','Macedonian','Malay',
		'Maltese','Mongolian','Norwegian','Persian','Polish','Portuguese','Romanian',
		'Russian','Serbian','Slovak','Slovenian','Spanish','Swedish','Tagalog','Tamil',
		'Telugu','Thai','Turkish','Ukrainian','Urdu','Uzbek','Vietnamese','Welsh',
	];

	$: langSuggestions = langInput.length > 0
		? LANGUAGES.filter(l => l.toLowerCase().startsWith(langInput.toLowerCase()) && !form.languages.includes(l))
		: [];

	// ── Onboarding state ────────────────────────────────────────────────────────
	let obStep = 1; // 1=keypair, 2=name, 3=done
	let obGenerating = false;

	$: if ($appReady && obStep === 1) {
		if ($identity && $profile?.display_name) obStep = 3;
		else if ($identity) obStep = 2;
	}

	async function obGenerateKeypair() {
		obGenerating = true;
		try {
			const info = await generateKeypair();
			identity.set(info);
			obStep = 2;
		} catch (e) { toast(String(e), 'error'); }
		finally { obGenerating = false; }
	}

	async function obSaveName() {
		if (!form.display_name.trim()) return;
		saving = true;
		try {
			form.updated = new Date().toISOString();
			await saveProfile(form);
			profile.set({ ...form });
			obStep = 3;
		} catch (e) { toast(String(e), 'error'); }
		finally { saving = false; }
	}

	// ── Publish-button dirty tracking ───────────────────────────────────────────
	// Snapshot of the profile as it was last published (undefined = never published).
	let publishedSnapshot: string | null = null;

	onMount(async () => {
		const wasPublished = await hasPublishedProfile().catch(() => false);
		if (wasPublished && $profile) {
			publishedSnapshot = stableProfileJson($profile);
		}
	});

	function stableProfileJson(p: Profile): string {
		// Exclude auto-computed fields that change on every save.
		const { updated, est_size, ...rest } = p;
		return JSON.stringify(rest);
	}

	$: profileDirty = publishedSnapshot === null || stableProfileJson(form) !== publishedSnapshot;

	// ── Disk size computation ────────────────────────────────────────────────────
	function formatBytes(b: number): string {
		const GB = 1073741824, MB = 1048576, KB = 1024;
		if (b >= GB) return (b / GB).toFixed(1) + ' GB';
		if (b >= MB) return (b / MB).toFixed(1) + ' MB';
		if (b >= KB) return (b / KB).toFixed(1) + ' KB';
		return b + ' B';
	}

	$: totalBytes = $collections.reduce((s, c) => s + (c.total_bytes ?? 0), 0);
	$: diskSize = totalBytes > 0 ? formatBytes(totalBytes) : '—';

	// ── Regular state ────────────────────────────────────────────────────────────
	let scanOpen = false;
	let scanTitle = 'Add collection';
	let scanInitialPath = '';
	let scanInitialAlias = '';
	let saving = false;
	let publishing = false;
	let shareSlug = '';
	let shareOpen = false;
	let langInput = '';

	const SOCIAL_PLATFORMS = [
		{ value: 'reddit',   label: 'Reddit',   abbr: 'r/' },
		{ value: 'discord',  label: 'Discord',  abbr: 'DC' },
		{ value: 'matrix',   label: 'Matrix',   abbr: '[M]' },
		{ value: 'bluesky',  label: 'Bluesky',  abbr: 'BS' },
		{ value: 'mastodon', label: 'Mastodon', abbr: 'MT' },
		{ value: 'github',   label: 'GitHub',   abbr: 'GH' },
		{ value: 'twitter',  label: 'Twitter/X',abbr: 'X' },
		{ value: 'other',    label: 'Other',    abbr: '···' },
	];

	let activeSocialPlatform: string | null = null;

	let form: Profile = {
		display_name: '',
		bio: undefined,
		tags: [],
		since: undefined,
		est_size: undefined,
		languages: [],
		contact_hint: undefined,
		email: undefined,
		location: undefined,
		social_links: [],
		updated: new Date().toISOString(),
	};

	function toggleSocialPlatform(platform: string) {
		activeSocialPlatform = activeSocialPlatform === platform ? null : platform;
	}

	function setSocialHandle(platform: string, handle: string) {
		const idx = form.social_links.findIndex(l => l.platform === platform);
		if (handle.trim()) {
			if (idx >= 0) {
				form.social_links[idx].handle = handle;
				form.social_links = form.social_links;
			} else {
				form.social_links = [...form.social_links, { platform, handle }];
			}
		} else {
			if (idx >= 0) form.social_links = form.social_links.filter((_, i) => i !== idx);
		}
	}

	function removeSocialByPlatform(platform: string) {
		form.social_links = form.social_links.filter(l => l.platform !== platform);
		activeSocialPlatform = null;
	}

	// Persist form in store across navigation — load from homeDraft first, then $profile.
	let profileLoaded = false;
	$: if ($appReady && !profileLoaded) {
		form = $homeDraft ?? ($profile ? { ...$profile } : form);
		profileLoaded = true;
	}
	// Keep homeDraft in sync whenever form changes.
	$: if (profileLoaded) homeDraft.set({ ...form });

	$: nameInitial = form.display_name?.[0]?.toUpperCase() ?? 'Y';
	$: nameHue = avatarHue(nameInitial);

	async function handleSave() {
		if (!form.display_name) return;
		saving = true;
		try {
			form.updated = new Date().toISOString();
			form.est_size = totalBytes > 0 ? diskSize : undefined;
			await saveProfile(form);
			profile.set({ ...form });
			toast('Profile saved');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			saving = false;
		}
	}

	async function handlePublish() {
		if (!form.display_name) return;
		publishing = true;
		try {
			form.updated = new Date().toISOString();
			form.est_size = totalBytes > 0 ? diskSize : undefined;
			await saveProfile(form);
			profile.set({ ...form });
			await publishProfile();
			publishedSnapshot = stableProfileJson(form);
			toast('Profile published to relay');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			publishing = false;
		}
	}

	async function handlePublishCollection(slug: string) {
		try {
			await publishCollection(slug);
			collections.update(cols => cols.map(c => c.slug === slug ? { ...c, published: true } : c));
			toast('Collection published');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// ── Collection language / notes / sorted management ──────────────────────────
	let colLangInputs: Record<string, string> = {};
	let colNotes: Record<string, string> = {};
	let colSorted: Record<string, boolean> = {};
	$: $collections.forEach(c => {
		if (!(c.slug in colLangInputs)) colLangInputs[c.slug] = '';
		if (!(c.slug in colNotes)) colNotes[c.slug] = c.description ?? '';
		if (!(c.slug in colSorted)) colSorted[c.slug] = c.sorted ?? false;
	});

	async function saveColMeta(col: import('$lib/types.js').Collection) {
		const slug = col.slug;
		const desc = (colNotes[slug] ?? '').trim() || undefined;
		const sorted = colSorted[slug] ?? false;
		try {
			await updateCollectionMeta(slug, desc, col.content_type, col.languages ?? [], sorted);
			collections.update(cols => cols.map(c =>
				c.slug === slug ? { ...c, description: desc, sorted } : c
			));
		} catch (e) { toast(String(e), 'error'); }
	}

	async function addColLang(slug: string, langStr: string) {
		const lang = langStr.trim();
		if (!lang) return;
		const col = $collections.find(c => c.slug === slug);
		if (!col) return;
		const langs = col.languages ?? [];
		if (langs.includes(lang)) { colLangInputs[slug] = ''; return; }
		const newLangs = [...langs, lang];
		colLangInputs[slug] = '';
		try {
			await updateCollectionMeta(slug, col.description, col.content_type, newLangs, colSorted[slug] ?? false);
			collections.update(cols => cols.map(c => c.slug === slug ? { ...c, languages: newLangs } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	async function removeColLang(slug: string, lang: string) {
		const col = $collections.find(c => c.slug === slug);
		if (!col) return;
		const newLangs = (col.languages ?? []).filter(l => l !== lang);
		try {
			await updateCollectionMeta(slug, col.description, col.content_type, newLangs, colSorted[slug] ?? false);
			collections.update(cols => cols.map(c => c.slug === slug ? { ...c, languages: newLangs } : c));
		} catch (e) { toast(String(e), 'error'); }
	}

	async function handleDeleteCollection(slug: string) {
		try {
			await deleteCollection(slug);
			collections.update((cols) => cols.filter((c) => c.slug !== slug));
			toast('Collection removed');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	function onScanned(event: CustomEvent<Collection>) {
		const col = event.detail;
		collections.update((cols) => {
			const idx = cols.findIndex((c) => c.slug === col.slug);
			if (idx >= 0) {
				const next = [...cols];
				next[idx] = col;
				return next;
			}
			return [...cols, col];
		});
		toast(`Scanned "${col.path_alias}" — ${col.item_count} items`);
	}

	function openShare(slug: string) {
		shareSlug = slug;
		shareOpen = true;
	}

	function openAddScan() {
		scanTitle = 'Add collection';
		scanInitialPath = '';
		scanInitialAlias = '';
		scanOpen = true;
	}

	async function openRescan(col: import('$lib/types.js').Collection) {
		scanTitle = 'Rescan collection';
		scanInitialAlias = col.path_alias;
		try {
			const share = await getShareSettings(col.slug);
			scanInitialPath = share?.root_path ?? '';
		} catch {
			scanInitialPath = '';
		}
		scanOpen = true;
	}

	$: totalItems = $collections.reduce((s, c) => s + c.item_count, 0);

	function addLang(name: string) {
		if (!form.languages.includes(name)) {
			form.languages = [...form.languages, name];
		}
		langInput = '';
	}

	function handleLangKey(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ',') {
			e.preventDefault();
			const raw = langInput.trim().replace(/,$/, '');
			const match = LANGUAGES.find(l => l.toLowerCase() === raw.toLowerCase())
				?? (langSuggestions.length === 1 ? langSuggestions[0] : null);
			if (match) addLang(match);
			// silently discard unrecognized input
		} else if (e.key === 'Backspace' && !langInput && form.languages.length > 0) {
			form.languages = form.languages.slice(0, -1);
		} else if (e.key === 'Tab' && langSuggestions.length > 0) {
			e.preventDefault();
			addLang(langSuggestions[0]);
		}
	}

	function removeLang(i: number) {
		form.languages = form.languages.filter((_, idx) => idx !== i);
	}
</script>

{#if obStep < 3}
	<!-- Onboarding flow -->
	<div class="onboarding">
		<div class="ob-logo">H</div>
		<div class="ob-text">
			<div class="ob-title">Welcome to Hoardbook</div>
			<div class="ob-sub">A peer-to-peer field guide for data hoarders. Publish what you keep, find others who keep it too.</div>
		</div>
		<div class="ob-card">
			<div class="ob-card-head">
				<span class="sect-label">Step {obStep} of 2</span>
				<div class="ob-dots">
					<div class="ob-dot" class:ob-dot-active={obStep === 1} class:ob-dot-done={obStep > 1} />
					<div class="ob-dot" class:ob-dot-active={obStep === 2} class:ob-dot-done={obStep > 2} />
				</div>
			</div>

			{#if obStep === 1}
				<div class="ob-card-title">Create your identity</div>
				<div class="ob-card-sub">Hoardbook uses a local Ed25519 keypair as your identity. No email, no server account.</div>
				<div class="ob-notice">
					<span class="ob-notice-icon">{@html icons.shield}</span>
					<div class="ob-notice-text">Your private key is stored locally and never transmitted. Back it up somewhere safe.</div>
				</div>
				<button class="btn-primary btn-full" on:click={obGenerateKeypair} disabled={obGenerating}>
					{obGenerating ? 'Generating…' : 'Generate keypair'}
				</button>
			{:else if obStep === 2}
				<div class="ob-card-title">Name yourself</div>
				<div class="ob-card-sub">Pick a display name. This is what others see when they look you up. You can add more profile details later.</div>
				<div class="field" style="margin-bottom:16px">
					<label class="field-label" for="ob-name">Display name <span class="accent-dot">•</span></label>
					<input id="ob-name" class="hb-input" type="text" placeholder="e.g. DataHoarder_42"
						bind:value={form.display_name}
						on:keydown={(e) => e.key === 'Enter' && obSaveName()} />
				</div>
				<button class="btn-primary btn-full" on:click={obSaveName} disabled={!form.display_name.trim() || saving}>
					{saving ? 'Saving…' : 'Get started →'}
				</button>
			{/if}
		</div>
	</div>
{:else}
	<!-- TopBar -->
	<div class="topbar">
		<div>
			<div class="topbar-title">My Profile</div>
			<div class="topbar-sub">Visible to anyone with your hb_id</div>
		</div>
		<div class="topbar-actions">
			<button class="btn-ghost btn-sm" on:click={handleSave} disabled={!form.display_name || saving}>
				{saving ? 'Saving…' : 'Save draft'}
			</button>
			<button class="btn-primary btn-sm" on:click={handlePublish} disabled={publishing || !profileDirty} title={!profileDirty ? 'No changes since last publish' : undefined}>
				{publishing ? 'Publishing…' : profileDirty ? 'Publish profile' : 'Published ✓'}
			</button>
		</div>
	</div>

	<div class="body">
		<!-- Left: Profile editor -->
		<div class="profile-pane">
			<div class="profile-header">
				<Avatar letter={nameInitial} size={48} hue={nameHue} />
				<div class="profile-header-info">
					<div class="profile-name">{form.display_name || 'DataHoarder'}</div>
					<span class="mono">{$identity?.hb_id_short ?? ''}</span>
				</div>
			</div>

			<div class="fields">
				<div class="field">
					<label class="field-label">Display name <span class="accent-dot">•</span></label>
					<input class="hb-input" type="text" placeholder="e.g. DataHoarder_42" bind:value={form.display_name} />
				</div>

				<div class="field">
					<label class="field-label">Bio</label>
					<textarea class="hb-input hb-textarea" rows="3" placeholder="What do you collect?" bind:value={form.bio}></textarea>
				</div>

				<div class="field-row">
					<div class="field">
						<label class="field-label">Hoarding since</label>
						<input class="hb-input" type="number" min="1990" max="2099" placeholder="2003" bind:value={form.since} />
					</div>
					<div class="field">
						<span class="field-label">Disk size (auto)</span>
						<span class="field-readonly">{diskSize}</span>
					</div>
				</div>

				<div class="field">
					<label class="field-label">Languages</label>
					<!-- svelte-ignore a11y-click-events-have-key-events -->
					<!-- svelte-ignore a11y-no-static-element-interactions -->
					<div class="lang-wrap-outer">
						<div class="tag-wrap" on:click={(e) => { if (e.target === e.currentTarget) e.currentTarget.querySelector('input')?.focus(); }}>
							{#each form.languages as lang, i}
								<span class="lang-tag">
									{lang}
									<button class="lang-x" on:click={() => removeLang(i)} title="Remove">×</button>
								</span>
							{/each}
							<input
								class="lang-input"
								type="text"
								placeholder={form.languages.length === 0 ? 'English, Japanese…' : 'Add…'}
								bind:value={langInput}
								on:keydown={handleLangKey}
							/>
						</div>
						{#if langSuggestions.length > 0}
							<div class="lang-suggestions">
								{#each langSuggestions.slice(0, 5) as s}
									<!-- svelte-ignore a11y-click-events-have-key-events -->
									<div class="lang-suggestion" on:click={() => addLang(s)} role="option" aria-selected="false">{s}</div>
								{/each}
							</div>
						{/if}
					</div>
				</div>

				<div class="field">
					<label class="field-label">Contact email</label>
					<input class="hb-input hb-input-mono" type="email" placeholder="you@example.com" bind:value={form.contact_hint} />
				</div>

				<div class="field">
					<label class="field-label">Region / City</label>
					<input class="hb-input" type="text" placeholder="Tokyo, EU/Germany, North America…" bind:value={form.location} />
				</div>

				<div class="field">
					<div class="social-icons-row">
						{#each SOCIAL_PLATFORMS as p}
							{@const link = form.social_links.find(l => l.platform === p.value)}
							<button
								class="social-icon-btn"
								class:social-icon-active={!!(link?.handle)}
								class:social-icon-selected={activeSocialPlatform === p.value}
								title={p.label + (link?.handle ? ': ' + link.handle : '')}
								on:click={() => toggleSocialPlatform(p.value)}
							>
								<span class="social-icon-abbr">{@html socialIcons[p.value] ?? p.abbr}</span>
								{#if link?.handle}<span class="social-icon-dot" />{/if}
							</button>
						{/each}
					</div>
					{#if activeSocialPlatform}
						{@const activePlat = SOCIAL_PLATFORMS.find(p => p.value === activeSocialPlatform)}
						{@const activeLink = form.social_links.find(l => l.platform === activeSocialPlatform)}
						<div class="social-edit-row">
							<span class="social-edit-label">{activePlat?.label}</span>
							<input
								class="hb-input social-handle"
								type="text"
								placeholder="username or handle"
								value={activeLink?.handle ?? ''}
								on:input={(e) => { if (activeSocialPlatform) setSocialHandle(activeSocialPlatform, e.currentTarget.value); }}
							/>
							{#if activeLink?.handle}
								<button class="social-remove" on:click={() => { if (activeSocialPlatform) removeSocialByPlatform(activeSocialPlatform); }} title="Remove">×</button>
							{/if}
						</div>
					{/if}
				</div>
			</div>
		</div>

		<!-- Right: Collections -->
		<div class="collections-pane">
			<div class="coll-head">
				<div>
					<div class="coll-title">Collections</div>
					<div class="coll-sub">{$collections.length} published · {totalItems.toLocaleString()} items</div>
				</div>
				<button class="btn-add" on:click={openAddScan}>
					<span>{@html icons.plus}</span>Add collection
				</button>
			</div>

			<!-- Stat strip -->
			<div class="stats">
				<div class="stat">
					<div class="stat-label">Items</div>
					<div class="stat-value">{totalItems.toLocaleString()}</div>
				</div>
				<div class="stat">
					<div class="stat-label">Collections</div>
					<div class="stat-value">{$collections.length}</div>
				</div>
				<div class="stat">
					<div class="stat-label">Total Size</div>
					<div class="stat-value">{diskSize}</div>
				</div>
			</div>

			<div class="coll-list">
				{#if $collections.length === 0}
					<div class="empty">No collections yet. Click "Add collection" to scan a directory.</div>
				{:else}
					{#each $collections as col}
						<CollectionPanel collection={col}>
							<!-- Language tags -->
							<div class="coll-lang-row">
								{#each (col.languages ?? []) as lang}
									<span class="lang-tag">
										{lang}
										<button class="lang-x" on:click={() => removeColLang(col.slug, lang)} title="Remove">×</button>
									</span>
								{/each}
								<input
									class="lang-input lang-input-sm"
									type="text"
									placeholder="+ language"
									bind:value={colLangInputs[col.slug]}
									on:keydown={(e) => {
										if (e.key === 'Enter' || e.key === ',') {
											e.preventDefault();
											addColLang(col.slug, colLangInputs[col.slug] ?? '');
										}
									}}
								/>
							</div>
							<!-- Notes + sorted -->
							<div class="coll-notes-row">
								<textarea
									class="coll-notes-input"
									rows="2"
									placeholder="Add notes about this collection (visible to peers)…"
									bind:value={colNotes[col.slug]}
									on:blur={() => saveColMeta(col)}
								></textarea>
								<label class="sorted-label">
									<input
										type="checkbox"
										class="sorted-check"
										bind:checked={colSorted[col.slug]}
										on:change={() => saveColMeta(col)}
									/>
									Sorted
								</label>
							</div>
							<div class="coll-actions">
								{#if !col.published}
									<span class="draft-badge">Draft</span>
								{/if}
								<button class="btn-ghost btn-sm" on:click={() => openRescan(col)}>Rescan</button>
								<button class="btn-ghost btn-sm" on:click={() => openShare(col.slug)}>Share</button>
								<button class="btn-ghost btn-sm" on:click={() => handlePublishCollection(col.slug)}>Publish</button>
								<button class="btn-ghost btn-sm btn-danger-ghost" on:click={() => handleDeleteCollection(col.slug)}>Remove</button>
							</div>
						</CollectionPanel>
					{/each}
				{/if}
			</div>
		</div>
	</div>

	<ScanDialog bind:open={scanOpen} title={scanTitle} initialPath={scanInitialPath} initialAlias={scanInitialAlias} on:scanned={onScanned} />
	<ShareSettingsDialog bind:open={shareOpen} slug={shareSlug} />
{/if}

<style>
	/* Onboarding */
	.onboarding {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		gap: 22px;
		padding: 40px;
		background: radial-gradient(circle at 50% 30%, var(--accent-soft) 0%, var(--bg) 60%);
	}

	.ob-logo {
		width: 56px; height: 56px;
		border-radius: 14px;
		background: linear-gradient(135deg, var(--accent) 0%, oklch(0.55 0.18 100) 100%);
		display: flex; align-items: center; justify-content: center;
		font-weight: 800; font-size: 28px; color: var(--accent-text);
		letter-spacing: -1.5px;
		box-shadow: 0 12px 40px -8px var(--accent-soft), inset 0 1px 0 oklch(1 0 0 / 0.2);
	}

	.ob-text { text-align: center; max-width: 380px; }

	.ob-title { font-size: 26px; font-weight: 700; letter-spacing: -0.6px; color: var(--fg); }

	.ob-sub { font-size: 14px; color: var(--fg-muted); margin-top: 8px; line-height: 1.55; }

	.ob-card {
		width: 400px;
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 22px;
	}

	.ob-card-head { margin-bottom: 16px; display: flex; justify-content: space-between; align-items: center; }

	.ob-dots { display: flex; gap: 6px; }
	.ob-dot {
		width: 8px; height: 8px; border-radius: 50%;
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		transition: background 0.2s;
	}
	.ob-dot-active { background: var(--accent); border-color: var(--accent); }
	.ob-dot-done { background: color-mix(in oklch, var(--accent) 40%, transparent); border-color: var(--accent); }

	/* Social links — icon row */
	.social-icons-row {
		display: flex; flex-wrap: wrap; gap: 5px; margin-bottom: 4px;
	}
	.social-icon-btn {
		position: relative;
		display: flex; align-items: center; justify-content: center;
		width: 38px; height: 28px;
		border-radius: 6px;
		background: var(--bg-elev2); border: 1px solid var(--border);
		cursor: pointer; font-family: var(--font-mono); color: var(--fg-muted);
		font-size: 10px; font-weight: 600; letter-spacing: -0.3px;
		transition: border-color 0.1s, background 0.1s, color 0.1s;
	}
	.social-icon-btn:hover { border-color: var(--fg-muted); color: var(--fg); }
	.social-icon-active { background: color-mix(in oklch, var(--accent) 12%, transparent); border-color: var(--accent); color: var(--accent); }
	.social-icon-selected { border-color: var(--accent); background: var(--accent-soft); }
	.social-icon-abbr { pointer-events: none; }
	.social-icon-dot {
		position: absolute; bottom: 3px; right: 3px;
		width: 4px; height: 4px; border-radius: 50%; background: var(--accent);
	}

	.social-edit-row { display: flex; gap: 6px; align-items: center; margin-top: 2px; }
	.social-edit-label {
		font-size: 11px; color: var(--fg-muted); font-weight: 500; white-space: nowrap; min-width: 54px;
	}

	.social-handle { flex: 1; }

	.social-remove {
		background: none; border: none; cursor: pointer; color: var(--fg-dim);
		font-size: 18px; line-height: 1; padding: 0 2px; display: flex; align-items: center;
		flex-shrink: 0;
	}
	.social-remove:hover { color: var(--fg); }

	.ob-card-title { font-size: 17px; font-weight: 600; color: var(--fg); margin-bottom: 6px; }


	.ob-card-sub { font-size: 12.5px; color: var(--fg-muted); margin-bottom: 18px; line-height: 1.5; }

	.ob-notice {
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 12px;
		margin-bottom: 16px;
		display: flex;
		gap: 10px;
		align-items: flex-start;
	}

	.ob-notice-icon { color: var(--accent); margin-top: 1px; flex-shrink: 0; }

	.ob-notice-text { font-size: 11.5px; color: var(--fg-muted); line-height: 1.5; }

	/* TopBar */
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

	/* Body layout */
	.body {
		display: flex;
		flex: 1;
		overflow: hidden;
	}

	/* Profile pane */
	.profile-pane {
		width: 320px;
		flex-shrink: 0;
		border-right: 1px solid var(--border);
		padding: 20px;
		overflow-y: auto;
		background: var(--bg);
	}

	.profile-header {
		display: flex;
		align-items: center;
		gap: 12px;
		margin-bottom: 18px;
	}

	.profile-header-info { flex: 1; min-width: 0; }

	.profile-name { font-size: 14px; font-weight: 600; color: var(--fg); }

	.fields { display: flex; flex-direction: column; gap: 12px; }

	.field { display: flex; flex-direction: column; gap: 5px; }

	.field-row { display: flex; gap: 10px; }

	.field-row .field { flex: 1; }

	/* Collections pane */
	.collections-pane {
		flex: 1;
		padding: 20px;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
	}

	.coll-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 4px;
		flex-shrink: 0;
	}

	.coll-title { font-size: 15px; font-weight: 600; }

	.coll-sub { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }

	.field-readonly {
		height: 34px;
		display: flex;
		align-items: center;
		padding: 0 11px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 7px;
		font-size: 13px;
		color: var(--fg-muted);
		font-feature-settings: 'tnum';
	}

	.stats {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 10px;
		margin: 16px 0 18px;
		flex-shrink: 0;
	}

	.stat {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 10px 12px;
	}

	.stat-label {
		font-size: 10.5px;
		color: var(--fg-dim);
		text-transform: uppercase;
		letter-spacing: 0.8px;
		font-weight: 600;
	}

	.stat-value {
		font-size: 18px;
		font-weight: 600;
		color: var(--fg);
		margin-top: 3px;
		letter-spacing: -0.3px;
		font-feature-settings: 'tnum';
	}

	.coll-list { display: flex; flex-direction: column; gap: 10px; }

	.coll-lang-row {
		display: flex;
		flex-wrap: wrap;
		gap: 4px;
		padding: 6px 10px 4px;
		border-top: 1px solid var(--divider);
		align-items: center;
	}

	.lang-input-sm {
		height: 22px;
		padding: 0 7px;
		font-size: 11px;
		min-width: 80px;
	}

	.coll-notes-row {
		display: flex;
		gap: 8px;
		padding: 6px 10px;
		border-top: 1px solid var(--divider);
		align-items: flex-start;
	}

	.coll-notes-input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-family: var(--font-ui);
		font-size: 11.5px;
		color: var(--fg);
		resize: none;
		line-height: 1.5;
		padding: 0;
	}
	.coll-notes-input::placeholder { color: var(--fg-dim); }

	.sorted-label {
		display: flex;
		align-items: center;
		gap: 5px;
		font-size: 11px;
		color: var(--fg-muted);
		cursor: pointer;
		flex-shrink: 0;
		padding-top: 2px;
		white-space: nowrap;
	}

	.sorted-check {
		accent-color: var(--accent);
		width: 13px;
		height: 13px;
		cursor: pointer;
	}

	.coll-actions {
		display: flex;
		gap: 4px;
		padding: 8px 10px;
		border-top: 1px solid var(--divider);
		align-items: center;
	}

	.draft-badge {
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.8px;
		color: oklch(0.75 0.14 60);
		background: oklch(0.25 0.06 60);
		border: 1px solid oklch(0.45 0.10 60 / 0.4);
		border-radius: 4px;
		padding: 1px 6px;
		flex-shrink: 0;
	}

	.empty { color: var(--fg-dim); font-size: 12.5px; text-align: center; padding: 32px 0; }

	/* Shared */
	.sect-label {
		font-size: 10.5px;
		color: var(--fg-dim);
		text-transform: uppercase;
		letter-spacing: 1.4px;
		font-weight: 600;
	}

	.mono {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
	}

	.field-label {
		font-size: 11px;
		color: var(--fg-muted);
		font-weight: 500;
		letter-spacing: 0.1px;
	}

	.accent-dot { color: var(--accent); margin-left: 3px; }

	.hb-input {
		display: flex;
		align-items: center;
		padding: 0 11px;
		height: 34px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		outline: none;
		width: 100%;
		transition: border-color 0.1s;
	}

	.hb-input:focus { border-color: var(--accent); }

	.hb-input::placeholder { color: var(--fg-dim); }

	.hb-input-mono { font-family: var(--font-mono); }

	/* Language tag input */
	.tag-wrap {
		display: flex;
		flex-wrap: wrap;
		gap: 5px;
		min-height: 34px;
		padding: 5px 8px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
		align-items: center;
		cursor: text;
		transition: border-color 0.1s;
	}
	.tag-wrap:focus-within { border-color: var(--accent); }
	.lang-tag {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 1px 5px 1px 7px;
		font-size: 11.5px;
		color: var(--fg);
		white-space: nowrap;
		font-family: var(--font-mono);
	}
	.lang-x {
		background: none;
		border: none;
		cursor: pointer;
		color: var(--fg-dim);
		font-size: 14px;
		line-height: 1;
		padding: 0;
		display: flex;
		align-items: center;
	}
	.lang-x:hover { color: var(--fg); }
	.lang-input {
		flex: 1;
		min-width: 50px;
		background: transparent;
		border: none;
		outline: none;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		padding: 0;
	}
	.lang-input::placeholder { color: var(--fg-dim); }

	.lang-wrap-outer { position: relative; }
	.lang-suggestions {
		position: absolute;
		top: calc(100% + 3px);
		left: 0; right: 0;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 7px;
		overflow: hidden;
		z-index: 10;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.25);
	}
	.lang-suggestion {
		padding: 7px 12px;
		font-size: 12.5px;
		color: var(--fg);
		cursor: pointer;
	}
	.lang-suggestion:hover { background: var(--bg-elev3); }

	.hb-textarea {
		height: auto;
		min-height: 64px;
		align-items: flex-start;
		padding: 9px 11px;
		resize: vertical;
	}

	/* Add collection button — dedicated class to avoid global style interference */
	.btn-add {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 6px;
		padding: 5px 11px;
		height: 28px;
		font-family: var(--font-ui);
		font-size: 12px;
		font-weight: 600;
		color: var(--accent-text);
		background: var(--accent);
		border: 1px solid var(--accent);
		border-radius: 7px;
		cursor: pointer;
		white-space: nowrap;
		line-height: 1;
		flex-shrink: 0;
	}

	/* Buttons */
	.btn-primary {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 5px 11px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; letter-spacing: -0.1px; white-space: nowrap; user-select: none;
		line-height: 1; height: 28px; flex-shrink: 0; text-decoration: none;
	}

	.btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

	.btn-ghost {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 5px 11px; font-family: var(--font-ui); font-size: 12px; font-weight: 500;
		color: var(--fg-muted); background: transparent;
		border: 1px solid transparent; border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none;
		line-height: 1; height: 28px; flex-shrink: 0;
	}

	.btn-ghost:disabled { opacity: 0.5; cursor: not-allowed; }

	.btn-danger-ghost { color: var(--error, #e05c5c); }
	.btn-danger-ghost:hover { background: color-mix(in oklch, var(--error, #e05c5c) 10%, transparent); }

	.btn-sm { padding: 5px 11px; font-size: 12px; height: 28px; }

	.btn-full { width: 100%; height: auto; padding: 10px 14px; }
</style>
