<script lang="ts">
	import { saveProfile, publishProfile, publishCollection, unpublishCollection, deleteCollection, exportCollection, getShareSettings, generateKeypair, hasPublishedProfile, backupData, importNsec } from '$lib/api.js';
	import { passphraseStrength } from '$lib/backup-export.js';
	import { save as saveDialog } from '@tauri-apps/plugin-dialog';
	import { profile, collections, identity, toast, appReady, homeDraft, identityLoadError } from '$lib/stores.js';
	import { onMount } from 'svelte';
	import { icons, socialIcons, avatarHue } from '$lib/icons.js';
	import ScanDialog from '$lib/components/ScanDialog.svelte';
	import AddCollectionModal from '$lib/components/AddCollectionModal.svelte';
	import CollectionRow from '$lib/components/CollectionRow.svelte';
	import Avatar from '$lib/components/Avatar.svelte';
	import HintMarker from '$lib/components/HintMarker.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import { applyProfilePicture, removeProfilePicture } from '$lib/profile-picture.js';
	import type { Collection, Profile } from '$lib/types.js';

	// Each language is shown in its own language (autonym).
	const LANGUAGES = [
		'Afrikaans','Shqip','العربية','Հայերեն','Azərbaycanca','Euskara','Беларуская',
		'বাংলা','Български','Català','中文','Hrvatski','Čeština','Dansk','Nederlands',
		'English','Eesti','Suomi','Français','Galego','ქართული','Deutsch','Ελληνικά',
		'עברית','हिन्दी','Magyar','Íslenska','Bahasa Indonesia','Italiano','日本語',
		'ಕನ್ನಡ','Қазақша','한국어','Latviešu','Lietuvių','Македонски','Bahasa Melayu',
		'Malti','Монгол','Norsk','فارسی','Polski','Português','Română',
		'Русский','Српски','Slovenčina','Slovenščina','Español','Svenska','Tagalog','தமிழ்',
		'తెలుగు','ไทย','Türkçe','Українська','اردو','Oʻzbekcha','Tiếng Việt','Cymraeg',
	];


	// ── Onboarding state ────────────────────────────────────────────────────────
	// 0=loading, 1=keypair, 2=name, 3=collection (skippable), 4=done
	let obStep = $state(0);
	let obGenerating = $state(false);
	let obKeypairRevealed = $state(false); // true after generation: show npub + share code + backup prompt
	let importOverlayOpen = $state(false); // linking-warning overlay before importing an existing nsec
	let obImportNsec = $state('');
	let obImportWarnAck = $state(false);
	let obImporting = $state(false);
	let obBackupPass = $state('');        // passphrase for the post-generate portable backup
	let obBackingUp = $state(false);


	async function obGenerateKeypair() {
		obGenerating = true;
		try {
			const info = await generateKeypair();
			identity.set(info);
			obKeypairRevealed = true; // stay on step 1 to show the share code + backup prompt
		} catch (e) { toast(String(e), 'error'); }
		finally { obGenerating = false; }
	}

	// Post-generate portable backup (whole profile, passphrase-encrypted). Replaces the legacy
	// key-only plaintext export.
	async function obExportBackup() {
		if (!obBackupStrength.acceptable) {
			toast(obBackupStrength.reason ?? 'Choose a stronger passphrase', 'error');
			return;
		}
		obBackingUp = true;
		try {
			const path = await saveDialog({
				defaultPath: 'hoardbook-backup.hbk',
				filters: [{ name: 'Hoardbook backup', extensions: ['hbk'] }],
			});
			if (!path) return; // dialog cancelled
			await backupData(obBackupPass, path);
			toast('Backup saved — keep it somewhere safe', 'success');
			obBackupPass = '';
			obStep = 2;
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			obBackingUp = false;
		}
	}

	// Import an existing Nostr key — ALWAYS behind the de-pseudonymization linking warning (there is
	// no offline oracle to tell whether a pasted key is public/Qurator).
	async function obImportExistingKey() {
		obImporting = true;
		try {
			const info = await importNsec(obImportNsec.trim());
			identity.set(info);
			importOverlayOpen = false;
			obImportNsec = '';
			obImportWarnAck = false;
			obKeypairRevealed = true;
		} catch (e) { toast(String(e), 'error'); }
		finally { obImporting = false; }
	}

	async function obSaveName() {
		if (!form.display_name.trim()) { obStep = 3; return; }
		saving = true;
		try {
			form.updated = new Date().toISOString();
			await saveProfile(form);
			profile.set({ ...form });
			obStep = 3;
		} catch (e) { toast(String(e), 'error'); }
		finally { saving = false; }
	}

	// ── Profile picture (devtest #1: surfaced on Home, where people actually look) ──────
	let pictureInput: HTMLInputElement | undefined = $state();
	let pictureBusy = $state(false);

	async function handlePictureFile(e: Event) {
		const file = (e.target as HTMLInputElement).files?.[0];
		if (pictureInput) pictureInput.value = ''; // allow re-picking the same file
		if (!file) return;
		pictureBusy = true;
		try {
			// devtest #4: the picture lives on the shared profile store, but the editor `form` is what
			// Save/Publish persist. Sync it back or the next publish rewrites `form` and reverts the image.
			if (await applyProfilePicture(file)) form.picture = $profile?.picture;
		} finally { pictureBusy = false; }
	}

	async function handleRemovePicture() {
		pictureBusy = true;
		try { await removeProfilePicture(); form.picture = $profile?.picture; } finally { pictureBusy = false; }
	}

	// ── Publish-button dirty tracking ───────────────────────────────────────────
	// Snapshot of the profile as it was last published (undefined = never published).
	let publishedSnapshot: string | null = $state(null);

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


	// ── Disk size computation ────────────────────────────────────────────────────
	function formatBytes(b: number): string {
		const GB = 1073741824, MB = 1048576, KB = 1024;
		if (b >= GB) return (b / GB).toFixed(1) + ' GB';
		if (b >= MB) return (b / MB).toFixed(1) + ' MB';
		if (b >= KB) return (b / KB).toFixed(1) + ' KB';
		return b + ' B';
	}


	// ── Regular state ────────────────────────────────────────────────────────────
	// The Rescan flow reuses the plain ScanDialog directly (single step — the collection's details
	// already exist). The Add-collection wizard (AddCollectionModal) owns the two-step Source→Details
	// flow, and also reopens straight to Details for "Edit details".
	let scanOpen = $state(false);
	let scanTitle = $state('Rescan collection');
	let scanInitialPath = $state('');
	let scanInitialAlias = $state('');
	let addModalOpen = $state(false);
	let editTarget: Collection | null = $state(null);
	let saving = $state(false);
	let publishing = $state(false);
	let langInput = $state('');
	let tagInput = $state('');
	let willingInput = '';

	const WILLING_OPTIONS = ['seed', 'trade', 'upload', 'meet up'];

	function addTag(raw: string) {
		const t = raw.trim().replace(/,$/, '').toLowerCase();
		if (t && !form.tags.includes(t)) form.tags = [...form.tags, t];
		tagInput = '';
	}

	function handleTagKey(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ',') {
			e.preventDefault();
			addTag(tagInput);
		} else if (e.key === 'Backspace' && !tagInput && form.tags.length > 0) {
			form.tags = form.tags.slice(0, -1);
		}
	}

	function removeTag(i: number) { form.tags = form.tags.filter((_, idx) => idx !== i); }

	function toggleWilling(opt: string) {
		if (form.willing_to.includes(opt)) {
			form.willing_to = form.willing_to.filter(w => w !== opt);
		} else {
			form.willing_to = [...form.willing_to, opt];
		}
	}

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

	let activeSocialPlatform: string | null = $state(null);

	let form: Profile = $state({
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
		willing_to: [],
		content_types: [],
		updated: new Date().toISOString(),
	});

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
	let profileLoaded = $state(false);


	async function handleSave() {
		// R1 (display name required to publish) does not apply to a draft — a blank name stays a
		// legal, unpublished draft. Trim before persisting so whitespace-only never sticks around.
		form.display_name = form.display_name.trim();
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
		if (!form.display_name.trim()) {
			toast('Enter a display name before publishing.', 'error');
			return;
		}
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
			const summary = await publishCollection(slug);
			collections.update(cols => cols.map(c => c.slug === slug ? { ...c, published: true } : c));
			// devtest #7: a too-large collection publishes only a truncated paywall teaser — tell the user
			// their browsers see a preview, not the whole thing.
			if (summary.truncated) {
				const hidden = Math.max(0, summary.total_items - summary.shown_items);
				toast(`Published a preview — this collection is too large to publish in full, so ${hidden.toLocaleString()} of ${summary.total_items.toLocaleString()} items are hidden from browsers.`);
			} else {
				toast('Collection published');
			}
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleUnpublishCollection(slug: string) {
		try {
			await unpublishCollection(slug);
			collections.update(cols => cols.map(c => c.slug === slug ? { ...c, published: false } : c));
			toast('Collection unpublished');
		} catch (e) {
			toast(String(e), 'error');
		}
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

	// Add-or-update a collection in the store — shared by the scan result (Add/Rescan) and the
	// wizard's Details step (Save draft / Publish), which each hand back a fresh Collection.
	function mergeCollectionIntoStore(col: Collection) {
		collections.update((cols) => {
			const idx = cols.findIndex((c) => c.slug === col.slug);
			if (idx >= 0) {
				const next = [...cols];
				next[idx] = col;
				return next;
			}
			return [...cols, col];
		});
	}

	function handleScannedCollection(collection: Collection) {
		mergeCollectionIntoStore(collection);
		toast(`Scanned "${collection.path_alias}" — ${collection.item_count} items`);
	}

	// The Details step already toasts its own "saved"/"published" message — just sync the store.
	function onWizardSaved(collection: Collection) {
		mergeCollectionIntoStore(collection);
	}
	function onWizardPublished(collection: Collection) {
		mergeCollectionIntoStore(collection);
	}

	function openAddModal() {
		editTarget = null;
		addModalOpen = true;
	}

	function openEditDetails(col: Collection) {
		editTarget = col;
		addModalOpen = true;
	}

	async function openRescan(col: Collection) {
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

	async function handleExport(slug: string, format: 'text' | 'markdown') {
		try {
			const text = await exportCollection(slug, format);
			await navigator.clipboard.writeText(text);
			toast('Copied to clipboard');
		} catch (e) { toast(String(e), 'error'); }
	}
	$effect(() => {
		if ($appReady && !profileLoaded) {
			form = $homeDraft ?? ($profile ? { ...$profile } : form);
			profileLoaded = true;
		}
	});
	let langSuggestions = $derived(langInput.length > 0
		? LANGUAGES.filter(l => l.toLowerCase().startsWith(langInput.toLowerCase()) && !form.languages.includes(l))
		: []);
	let obBackupStrength = $derived(passphraseStrength(obBackupPass));
	$effect(() => {
		if ($appReady && obStep === 0) {
			if ($identity) obStep = 4;
			else if (!$identityLoadError) obStep = 1;
			// identityLoadError set: stay at 0; the error screen is shown in the template.
		}
	});
	let profileDirty = $derived(publishedSnapshot === null || stableProfileJson(form) !== publishedSnapshot);
	// Never-published is the critical case: the profile isn't searchable until it's pushed to a relay.
	let neverPublished = $derived(publishedSnapshot === null);
	let totalBytes = $derived($collections.reduce((s, c) => s + (c.total_bytes ?? 0), 0));
	let diskSize = $derived(totalBytes > 0 ? formatBytes(totalBytes) : '—');
	// Keep homeDraft in sync whenever form changes.
	$effect(() => {
		if (profileLoaded) homeDraft.set({ ...form });
	});
	let nameInitial = $derived(form.display_name?.[0]?.toUpperCase() ?? 'Y');
	let nameHue = $derived(avatarHue(nameInitial));
	let totalItems = $derived($collections.reduce((s, c) => s + c.item_count, 0));
</script>

{#if obStep === 0}
	{#if $appReady && $identityLoadError}
		<div class="loading-screen">
			<div class="id-error-card">
				<div class="id-error-title">Identity file unreadable</div>
				<div class="id-error-body">Your keypair file exists but could not be decrypted. This usually means the Windows credentials used to encrypt it have changed.</div>
				<div class="id-error-detail">{$identityLoadError}</div>
				<a href="/settings" class="btn-primary btn-sm id-error-btn">Open Settings → Wipe data</a>
			</div>
		</div>
	{:else}
		<div class="loading-screen">
			<div class="loading-logo">
				<svg viewBox="0 0 18 24" width="24" height="32" style="overflow:visible" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
					<line x1="4" y1="-8" x2="4" y2="22"/>
					<path d="M4 12.5 C4 8 15 8 15 12.5 L15 22"/>
				</svg>
			</div>
		</div>
	{/if}
{:else if obStep < 4}
	<!-- Onboarding flow -->
	<div class="onboarding">
		<div class="ob-logo">
			<svg viewBox="0 0 18 24" width="30" height="40" style="overflow:visible" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
				<line x1="4" y1="-8" x2="4" y2="22"/>
				<path d="M4 12.5 C4 8 15 8 15 12.5 L15 22"/>
			</svg>
		</div>
		<div class="ob-text">
			<div class="ob-title">Welcome to Hoardbook</div>
			<div class="ob-sub">A peer-to-peer field guide for data hoarders. Publish what you keep, find others who keep it too.</div>
		</div>
		<div class="ob-card">
			<div class="ob-card-head">
				<span class="sect-label">Step {obStep} of 3</span>
				<div class="ob-dots">
					<div class="ob-dot" class:ob-dot-active={obStep === 1} class:ob-dot-done={obStep > 1}></div>
					<div class="ob-dot" class:ob-dot-active={obStep === 2} class:ob-dot-done={obStep > 2}></div>
					<div class="ob-dot" class:ob-dot-active={obStep === 3} class:ob-dot-done={obStep > 3}></div>
				</div>
			</div>

			{#if obStep === 1}
				{#if !obKeypairRevealed}
					<div class="ob-card-title">Create your identity</div>
					<div class="ob-card-sub">Hoardbook uses a Nostr key (your <span class="mono">npub</span>) as your identity. No email, no server account. It is stored encrypted on this device and never sent to a server.</div>
					<div class="ob-notice">
						<span class="ob-notice-icon">{@html icons.shield}</span>
						<div class="ob-notice-text">Your key is stored locally and never transmitted. There is no recovery if you lose it — so you'll back it up next.</div>
					</div>
					<button class="btn-primary btn-full" onclick={obGenerateKeypair} disabled={obGenerating}>
						{obGenerating ? 'Generating…' : 'Generate my Hoardbook identity'}
					</button>
					<button class="btn-ghost btn-full ob-skip" style="margin-top:8px" onclick={() => { importOverlayOpen = true; }}>
						Already have a Nostr key? Import
					</button>
				{:else}
					<div class="ob-card-title">Your identity is ready</div>
					<div class="ob-card-sub">This is your <strong>share code</strong>. Give it only to people you want browsing your collections — anyone holding it can decrypt your listings.</div>
					<div class="ob-hbid-row">
						<span class="ob-hbid mono">{$identity?.share_code ?? ''}</span>
						<button class="btn-ghost btn-sm" onclick={() => { navigator.clipboard.writeText($identity?.share_code ?? ''); toast('Copied', 'success'); }}>Copy</button>
					</div>
					<div class="ob-notice" style="margin-top:12px">
						<span class="ob-notice-icon">{@html icons.shield}</span>
						<div class="ob-notice-text">Export a backup now and store it somewhere safe. <strong>If you lose this key your identity is gone — there is no recovery.</strong></div>
					</div>
					<input class="hb-input" style="margin-top:10px" type="password" placeholder="Backup passphrase (min 12 characters)" bind:value={obBackupPass} />
					{#if obBackupPass && !obBackupStrength.acceptable}
						<div class="ob-card-sub" style="margin-top:4px">{obBackupStrength.reason ?? 'Choose a stronger passphrase.'}</div>
					{/if}
					<button class="btn-primary btn-full" style="margin-top:10px" onclick={obExportBackup} disabled={obBackingUp || !obBackupStrength.acceptable}>
						{obBackingUp ? 'Exporting…' : 'Export backup file'}
					</button>
					<button class="btn-ghost btn-full ob-skip" onclick={() => obStep = 2}>
						I'll do it later
					</button>
				{/if}
			{:else if obStep === 2}
				<div class="ob-card-title">Name yourself</div>
				<div class="ob-card-sub">Pick a display name — optional. You can change it and fill in the rest of your profile later from Home.</div>
				<div class="field" style="margin-bottom:16px">
					<label class="field-label" for="ob-name">Display name</label>
					<input id="ob-name" class="hb-input" type="text" placeholder="e.g. DataHoarder_42"
						bind:value={form.display_name}
						onkeydown={(e) => e.key === 'Enter' && obSaveName()} />
				</div>
				<button class="btn-primary btn-full" onclick={obSaveName} disabled={saving}>
					{saving ? 'Saving…' : form.display_name.trim() ? 'Continue →' : 'Skip'}
				</button>
				<button class="btn-ghost btn-full ob-skip" onclick={() => obStep = 3}>
					Skip
				</button>
			{:else if obStep === 3}
				<div class="ob-card-title">Add your first collection</div>
				<div class="ob-card-sub">Point Hoardbook at a folder to catalog what you keep. You can scan more folders later from the home screen.</div>
				<button class="btn-primary btn-full" onclick={() => { openAddModal(); obStep = 4; }}>
					{@html icons.folder} Scan a folder
				</button>
				<button class="btn-ghost btn-full ob-skip" onclick={() => obStep = 4}>
					Skip for now
				</button>
			{/if}
		</div>
	</div>

	{#if importOverlayOpen}
		<Modal open={true} title="Import an existing Nostr key" onclose={() => { importOverlayOpen = false; }}>
			<div class="ob-import-body">
				<div class="ob-card-sub" style="margin-bottom:16px">
					Paste your <span class="mono">nsec</span>. Hoardbook keeps the matching <span class="mono">npub</span>
					as your identity and mints a fresh transport key + browse-key. Your secret key is stored
					encrypted on this device and never sent to a server.
				</div>
				<div class="ob-notice">
					<span class="ob-notice-icon">{@html icons.shield}</span>
					<div class="ob-notice-text"><strong>Linking warning:</strong> if this is a public key — or the same
						key you use in Qurator or elsewhere — importing it links that identity to your Hoardbook
						activity and de-pseudonymizes you. Only continue if you understand this.</div>
				</div>
				<label class="ob-ack"><input type="checkbox" bind:checked={obImportWarnAck} /> I understand the linking implication.</label>
				<input class="hb-input mono" style="margin-top:10px" type="password" placeholder="nsec1…" bind:value={obImportNsec} />
				<button class="btn-primary btn-full" style="margin-top:10px" onclick={obImportExistingKey} disabled={!obImportWarnAck || !obImportNsec.trim() || obImporting}>
					{obImporting ? 'Importing…' : 'Import key'}
				</button>
				<button class="btn-ghost btn-full ob-skip" onclick={() => { importOverlayOpen = false; obImportNsec = ''; obImportWarnAck = false; }}>
					Cancel
				</button>
			</div>
		</Modal>
	{/if}
{:else}
	<!-- TopBar -->
	<div class="topbar">
		<div>
			<div class="topbar-title">My Profile</div>
			<div class="topbar-sub">
				{#if neverPublished}
					<span class="pub-status pub-warn">● Not published yet — others can't find you in search until you publish</span>
				{:else if profileDirty}
					<span class="pub-status pub-warn">● Unpublished changes — re-publish to update your public listing</span>
				{:else}
					<span class="pub-status pub-ok">● Published — you're discoverable in search</span>
				{/if}
			</div>
		</div>
		<div class="topbar-actions">
			<button class="btn-ghost btn-sm" onclick={handleSave} disabled={saving}>
				{saving ? 'Saving…' : 'Save draft'}
			</button>
			<button class="btn-primary btn-sm" class:publish-pulse={neverPublished && !publishing} onclick={handlePublish} disabled={publishing || !profileDirty || !form.display_name.trim()} title={!form.display_name.trim() ? 'Enter a display name before publishing' : !profileDirty ? 'No changes since last publish' : undefined}>
				{publishing ? 'Publishing…' : profileDirty ? 'Publish profile' : 'Published ✓'}
			</button>
		</div>
	</div>

	<div class="body">
		<!-- Left: Profile editor -->
		<div class="profile-pane">
			<div class="profile-header">
				<button
					type="button"
					class="avatar-picker"
					onclick={() => pictureInput?.click()}
					disabled={pictureBusy}
					title="Change profile picture"
					aria-label="Change profile picture"
				>
					<Avatar letter={nameInitial} size={48} hue={nameHue} picture={$profile?.picture} />
					<span class="avatar-picker-badge">{pictureBusy ? '…' : '📷'}</span>
				</button>
				<div class="profile-header-info">
					<div class="profile-name">{form.display_name || 'DataHoarder'}</div>
					<span class="mono">{$identity?.npub_short ?? ''}</span>
					{#if $profile?.picture}
						<button type="button" class="picture-remove-link" onclick={handleRemovePicture} disabled={pictureBusy}>
							Remove picture
						</button>
					{/if}
				</div>
				<input
					bind:this={pictureInput}
					type="file"
					accept="image/png,image/jpeg,image/webp"
					style="display:none"
					onchange={handlePictureFile}
				/>
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
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<div class="lang-wrap-outer">
						<div class="tag-wrap" onclick={(e) => { if (e.target === e.currentTarget) e.currentTarget.querySelector('input')?.focus(); }}>
							{#each form.languages as lang, i (lang)}
								<span class="lang-tag">
									{lang}
									<button class="lang-x" onclick={() => removeLang(i)} title="Remove">×</button>
								</span>
							{/each}
							<input
								class="lang-input"
								type="text"
								placeholder={form.languages.length === 0 ? 'English, 日本語…' : 'Add…'}
								bind:value={langInput}
								onkeydown={handleLangKey}
							/>
						</div>
						{#if langSuggestions.length > 0}
							<div class="lang-suggestions">
								{#each langSuggestions.slice(0, 5) as s (s)}
									<!-- svelte-ignore a11y_click_events_have_key_events -->
									<div class="lang-suggestion" onclick={() => addLang(s)} role="option" aria-selected="false">{s}</div>
								{/each}
							</div>
						{/if}
					</div>
				</div>

				<div class="field">
					<label class="field-label">Contact hint<HintMarker label="Contact hint" text="How people can reach you outside Hoardbook — a Reddit/Discord handle or an email. It is kept out of your public profile, and how your contacts re-find you if you ever lose your key." /></label>
					<input class="hb-input hb-input-mono" type="text" placeholder="u/you on Reddit · you@example.com" bind:value={form.contact_hint} />
				</div>

				<div class="field">
					<label class="field-label">Region / City<HintMarker label="Region / City" text="Optional — fill this in if you're interested in local meetups." /></label>
					<input class="hb-input" type="text" bind:value={form.location} />
				</div>

				<div class="field">
					<div class="social-icons-row">
						{#each SOCIAL_PLATFORMS as p (p.value)}
							{@const link = form.social_links.find(l => l.platform === p.value)}
							<button
								class="social-icon-btn"
								class:social-icon-active={!!(link?.handle)}
								class:social-icon-selected={activeSocialPlatform === p.value}
								title={p.label + (link?.handle ? ': ' + link.handle : '')}
								onclick={() => toggleSocialPlatform(p.value)}
							>
								<span class="social-icon-abbr">{@html socialIcons[p.value] ?? p.abbr}</span>
								{#if link?.handle}<span class="social-icon-dot"></span>{/if}
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
								oninput={(e) => { if (activeSocialPlatform) setSocialHandle(activeSocialPlatform, e.currentTarget.value); }}
							/>
							{#if activeLink?.handle}
								<button class="social-remove" onclick={() => { if (activeSocialPlatform) removeSocialByPlatform(activeSocialPlatform); }} title="Remove">×</button>
							{/if}
						</div>
					{/if}
				</div>

				<div class="field">
					<label class="field-label">Tags</label>
					<div class="tag-wrap">
						{#each form.tags as tag, i (tag)}
							<span class="lang-tag">{tag}<button class="lang-x" onclick={() => removeTag(i)} title="Remove">×</button></span>
						{/each}
						<input class="lang-input" type="text" placeholder="anime, scifi, docs…"
							bind:value={tagInput} onkeydown={handleTagKey} />
					</div>
				</div>

				<div class="field">
					<label class="field-label">Willing to</label>
					<div class="willing-row">
						{#each WILLING_OPTIONS as opt (opt)}
							<button class="willing-btn" class:willing-active={form.willing_to.includes(opt)}
								onclick={() => toggleWilling(opt)}>
								{opt}
							</button>
						{/each}
					</div>
				</div>

				{#if form.content_types.length > 0}
					<div class="field">
						<span class="field-label">Content types (auto)</span>
						<div class="badge-row-sm">
							{#each form.content_types as ct (ct)}
								<span class="ct-badge">{ct}</span>
							{/each}
						</div>
						<div class="field-hint">Auto-computed from your published collections.</div>
					</div>
				{/if}
			</div>
		</div>

		<!-- Right: Collections -->
		<div class="collections-pane">
			<div class="coll-head">
				<div>
					<div class="coll-title">Collections</div>
					<div class="coll-sub">{$collections.filter(c => c.published).length} of {$collections.length} published · {totalItems.toLocaleString()} items</div>
				</div>
				<button class="btn-add" onclick={openAddModal}>
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
					<div class="empty">No collections yet. Click "Add collection" to scan a folder.</div>
				{:else}
					{#each $collections as col (col.slug)}
						<CollectionRow
							collection={col}
							onrescan={() => openRescan(col)}
							onedit={() => openEditDetails(col)}
							onpublish={() => handlePublishCollection(col.slug)}
							onunpublish={() => handleUnpublishCollection(col.slug)}
							onremove={() => handleDeleteCollection(col.slug)}
							onexport={(detail) => handleExport(detail.slug, detail.format)}
						/>
					{/each}
				{/if}
			</div>
		</div>
	</div>

	<ScanDialog bind:open={scanOpen} title={scanTitle} initialPath={scanInitialPath} initialAlias={scanInitialAlias} onscanned={handleScannedCollection} />
	<AddCollectionModal bind:open={addModalOpen} editCollection={editTarget} onscanned={handleScannedCollection} onsaved={onWizardSaved} onpublished={onWizardPublished} />
{/if}

<style>
	/* Loading screen */
	.loading-screen {
		display: flex;
		height: 100%;
		align-items: center;
		justify-content: center;
	}

	.loading-logo {
		width: 40px; height: 40px;
		border-radius: 10px;
		background: var(--bg-elev3);
		border: 1px solid color-mix(in oklch, var(--accent) 22%, transparent);
		display: flex; align-items: center; justify-content: center;
		color: var(--accent);
		overflow: hidden;
		opacity: 0.6;
		animation: pulse 1.4s ease-in-out infinite;
	}

	@keyframes pulse {
		0%, 100% { opacity: 0.4; transform: scale(0.95); }
		50% { opacity: 0.8; transform: scale(1); }
	}

	/* Identity load error */
	.id-error-card {
		width: 380px;
		background: var(--bg-elev1);
		border: 1px solid oklch(0.65 0.18 25 / 0.35);
		border-radius: 10px;
		padding: 22px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.id-error-title { font-size: 15px; font-weight: 600; color: var(--fg); }
	.id-error-body { font-size: 13px; color: var(--fg-muted); line-height: 1.55; }
	.id-error-detail {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: oklch(0.75 0.12 25);
		background: oklch(0.20 0.04 25 / 0.5);
		border: 1px solid oklch(0.45 0.12 25 / 0.3);
		border-radius: 6px;
		padding: 8px 10px;
		word-break: break-word;
	}
	.id-error-btn { align-self: flex-start; text-decoration: none; margin-top: 4px; }

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
		background: var(--bg-elev3);
		border: 1px solid color-mix(in oklch, var(--accent) 28%, transparent);
		display: flex; align-items: center; justify-content: center;
		color: var(--accent);
		overflow: hidden;
		box-shadow: 0 8px 28px -6px oklch(0.78 0.14 70 / 0.18);
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

	/* HB-ID display row in step 1 post-generate */
	.ob-hbid-row {
		display: flex;
		align-items: center;
		gap: 8px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 8px 10px;
		margin-bottom: 12px;
	}

	.ob-hbid {
		flex: 1;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg);
		word-break: break-all;
		line-height: 1.5;
	}

	/* M15 W2: Qurator import overlay now uses Modal.svelte; body is a plain content column. */
	.ob-import-body { display: flex; flex-direction: column; }

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

	/* Publish-state hint + attention pulse — make "unpublished = not searchable" obvious. */
	.pub-status { font-size: 12px; }
	.pub-warn { color: var(--accent); }
	.pub-ok { color: var(--fg-dim); }

	.publish-pulse { animation: publish-pulse 1.8s ease-out infinite; }
	/* M15 W7: pulse uses the amber accent, not a stray blue. */
	@keyframes publish-pulse {
		0%   { box-shadow: 0 0 0 0 oklch(0.78 0.14 70 / 0.55); }
		70%  { box-shadow: 0 0 0 7px oklch(0.78 0.14 70 / 0); }
		100% { box-shadow: 0 0 0 0 oklch(0.78 0.14 70 / 0); }
	}
	@media (prefers-reduced-motion: reduce) { .publish-pulse { animation: none; } }

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

	/* devtest #1: the avatar itself is the picture-picker on Home (the discoverable spot). */
	.avatar-picker {
		position: relative;
		padding: 0;
		border: none;
		background: none;
		cursor: pointer;
		border-radius: 50%;
		line-height: 0;
		flex-shrink: 0;
	}
	.avatar-picker:disabled { cursor: default; opacity: 0.7; }
	.avatar-picker-badge {
		position: absolute;
		right: -2px;
		bottom: -2px;
		width: 18px;
		height: 18px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 10px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 50%;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
	}
	.avatar-picker:hover:not(:disabled) .avatar-picker-badge { border-color: var(--accent); }
	.picture-remove-link {
		margin-top: 2px;
		padding: 0;
		border: none;
		background: none;
		font-size: 11px;
		color: var(--fg-muted);
		cursor: pointer;
		text-decoration: underline;
	}
	.picture-remove-link:hover:not(:disabled) { color: var(--fg); }
	.picture-remove-link:disabled { cursor: default; opacity: 0.6; }

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

	.willing-row { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 4px; }

	.willing-btn {
		font-size: 11.5px;
		padding: 4px 10px;
		border-radius: 5px;
		border: 1px solid var(--border);
		background: var(--bg-elev2);
		color: var(--fg-muted);
		cursor: pointer;
		font-family: inherit;
		transition: background 0.1s, color 0.1s, border-color 0.1s;
	}

	.willing-btn:hover { background: var(--bg-elev3); }

	.willing-active {
		background: color-mix(in oklch, var(--accent) 14%, transparent) !important;
		color: var(--accent) !important;
		border-color: color-mix(in oklch, var(--accent) 30%, transparent) !important;
	}

	.badge-row-sm { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 4px; }

	.ct-badge {
		font-size: 11px;
		padding: 2px 8px;
		border-radius: 4px;
		background: color-mix(in oklch, var(--accent) 10%, transparent);
		color: var(--accent);
		border: 1px solid color-mix(in oklch, var(--accent) 20%, transparent);
		text-transform: capitalize;
	}

	.field-hint { font-size: 11px; color: var(--fg-dim); margin-top: 4px; }

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

	/* M15 W1: core buttons unified on the app.css .btn system (local copies removed). `.btn-add`
	   (compact publish button) and `.btn-full`'s taller onboarding CTA sizing stay local. */
	.btn-full { height: auto; padding: 10px 14px; } /* taller onboarding CTA (width comes from app.css) */
	.ob-skip { margin-top: 4px; font-size: 12px; }
</style>
