<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { generateKeypair, getSettings, saveSettings, importNsec, backupData, peekBackup, restoreData, validateBackup, wipeData, checkRelay, relayStatus, beaconStatus, checkUpdate, downloadUpdate, applyStagedUpdate, takeUpdateNotice, updaterIsPortable, checkPortableUpdate, applyPortableUpdate, hasPublishedProfile, publishProfile, copyDiagnostics, revealLogFolder, natClassification, dmBlockedList, dmUnblock, dmBlock, validateShareCode, shareCodeInfo } from '$lib/api.js';
	import type { Settings, UpdateInfo, PortableUpdateInfo, BeaconReport, NatClassification } from '$lib/api.js';
	import { keyView } from '$lib/key-view.js';
	import { shortNpub } from '$lib/contact-display.js';
	import { passphraseStrength, backupModeOptions, type BackupMode } from '$lib/backup-export.js';
	import { updateNoticeVM } from '$lib/update-ux.js';
	import { beaconLine, loopLine } from '$lib/beacon-view.js';
	import { DEFAULT_RELAYS, validateRelayUrl } from '$lib/relays.js';
	import { relaunch } from '@tauri-apps/plugin-process';
	import { open as openFileDialog, save as saveFileDialog, confirm } from '@tauri-apps/plugin-dialog';
	import { getVersion } from '@tauri-apps/api/app';
	import { identity, profile, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import FeatureTooltip from '$lib/components/FeatureTooltip.svelte';

	let generating = $state(false);
	let copied = $state(false);
	let appVersion = $state('');

	// Full settings object, preserved so saving one field never resets the others (the M5 fields
	// privacy_notice_acknowledged / last_seen_version + the M9 snapshot/online toggles live here too).
	let settings: Settings = $state({
		relay_urls: [], allow_dms: true, privacy_notice_acknowledged: false,
		last_seen_version: '',
		snapshot_auto_update: true, snapshot_reconcile_poll: false, show_online_count: true,
		discoverable: false,
		big_relay_url: '',
	});

	// ── 3-key identity view ───────────────────────────────────────────────────────
	let kv = $derived($identity ? keyView($identity) : null);

	// ── Backup / restore ─────────────────────────────────────────────────────────
	const backupModes = backupModeOptions();
	let backupMode: BackupMode = $state('passphrase');
	let backupPass = $state('');
	let backingUp = $state(false);
	let backupStrength = $derived(passphraseStrength(backupPass));

	async function handleBackup() {
		if (backupMode === 'passphrase' && !backupStrength.acceptable) {
			toast(backupStrength.reason ?? 'Choose a stronger passphrase', 'error');
			return;
		}
		const path = await saveFileDialog({
			defaultPath: 'hoardbook-backup.hbk',
			filters: [{ name: 'Hoardbook backup', extensions: ['hbk'] }],
		});
		if (!path) return;
		if (backupMode === 'plaintext') {
			const ok = await confirm(
				'This backup is unencrypted. The file is your identity, and anyone who has it becomes you. Store it like a master key. Continue?',
				{ title: 'Plaintext backup', kind: 'warning' },
			);
			if (!ok) return;
		}
		backingUp = true;
		try {
			await backupData(backupMode === 'passphrase' ? backupPass : null, path);
			toast('Backup saved', 'success');
			backupPass = '';
		} catch (e) { toast(String(e), 'error'); }
		finally { backingUp = false; }
	}

	// Restore: pick a file → peek (does it need a passphrase?) → confirm wipe → restore → relaunch.
	let restoreNeedsPass = $state(false);
	let restorePass = $state('');
	let restorePath: string | null = $state(null);
	let restoring = $state(false);

	async function pickRestore() {
		const path = await openFileDialog({
			multiple: false,
			filters: [{ name: 'Hoardbook backup', extensions: ['hbk', 'json'] }],
		});
		if (!path) return;
		try {
			restoreNeedsPass = await peekBackup(path as string);
		} catch (e) { toast(`Not a valid Hoardbook backup: ${String(e)}`, 'error'); return; }
		restorePath = path as string;
		restorePass = '';
		if (!restoreNeedsPass) doRestore();
	}

	async function doRestore() {
		// Re-entry guard, FIRST: two clicks (or the pickRestore auto-call racing a manual click)
		// can both be parked in `confirm` before either sets `restoring`, starting two destructive
		// flows. The button's `disabled={… || restoring}` cannot help there — `restoring` is still
		// false while the dialog is open — so the guard has to live here.
		if (restoring) return;
		if (!restorePath) return;
		// QURATOR-126 (INV-8): capture the inputs ONCE, before any await. Everything downstream —
		// including the wipe — must be about the archive that gets validated, not whatever the
		// still-mounted reactive fields happen to hold mid-flight; re-reading them after `wipeData`
		// let a mid-await edit (or Cancel nulling `restorePath`) orphan a wiped device.
		const pass = restoreNeedsPass ? restorePass : null;
		const path = restorePath;
		restoring = true;
		const ok = await confirm(
			'Restoring REPLACES all current data on this device with the backup, then restarts. Continue?',
			{ title: 'Restore from backup', kind: 'warning' },
		);
		if (!ok) { restoring = false; return; }
		try {
			// QURATOR-126 (INV-8): prove the backup is restorable — KDF, decrypt, parse — BEFORE
			// wiping. A typo'd passphrase used to destroy the local identity with no rollback.
			await validateBackup(pass, path);
			await wipeData();
			const info = await restoreData(pass, path);
			identity.set(info);
			restorePath = null; restorePass = ''; restoreNeedsPass = false;
			toast('Backup restored. Restarting…');
			await new Promise(r => setTimeout(r, 2500));
			await relaunch();
		} catch (e) { toast(String(e), 'error'); restoring = false; }
	}

	// ── Import a different Nostr key (always warns about linking) ─────────────────
	let importOpen = $state(false);
	let importNsecValue = $state('');
	let importWarnAck = $state(false);
	let importingNsec = $state(false);

	async function handleImportNsec() {
		importingNsec = true;
		try {
			const info = await importNsec(importNsecValue.trim());
			identity.set(info);
			importOpen = false; importNsecValue = ''; importWarnAck = false;
			toast('Nostr key imported');
		} catch (e) { toast(String(e), 'error'); }
		finally { importingNsec = false; }
	}

	// ── Updates (Obsidian deferred-install) ──────────────────────────────────────
	let updateChecking = $state(false);
	let updateStaging = $state(false);
	let updateInfo: UpdateInfo | null = $state(null);
	let updateChecked = $state(false);
	let updateError = $state('');
	let stagedVersion: string | null = $state(null);
	// Portable self-updater: the loose-exe build routes here instead of the NSIS staged flow. Detected
	// once in onMount; a portable update is a single download→verify→swap→restart (no deferred install).
	let isPortable = $state(false);
	let portableInfo: PortableUpdateInfo | null = $state(null);
	let portableApplying = $state(false);

	async function doCheckUpdate() {
		updateChecking = true;
		updateError = '';
		updateInfo = null;
		portableInfo = null;
		updateChecked = false;
		try {
			if (isPortable) {
				portableInfo = await checkPortableUpdate();
			} else {
				updateInfo = await checkUpdate();
			}
			updateChecked = true;
		} catch (e) {
			updateError = String(e).replace(/^Error: /, '');
		} finally {
			updateChecking = false;
		}
	}

	// Portable apply: download + minisign-verify + self-replace the running exe, then relaunch (so on
	// success control never returns here). A failure — bad signature, network, no artifact — toasts.
	async function doApplyPortable() {
		portableApplying = true;
		try {
			await applyPortableUpdate();
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			portableApplying = false;
		}
	}

	// Background download + minisign-verify, staged for deferred install — NO immediate restart.
	async function doDownloadUpdate() {
		updateStaging = true;
		try {
			stagedVersion = await downloadUpdate();
			if (stagedVersion) {
				toast(`Update v${stagedVersion} downloaded. It installs when you restart.`, 'success');
			}
		} catch (e) { toast(String(e), 'error'); }
		finally { updateStaging = false; }
	}

	async function doApplyUpdate() {
		try { await applyStagedUpdate(); } catch (e) { toast(String(e), 'error'); }
	}

	// ── Diagnostics (QURATOR-65) ─────────────────────────────────────────────────
	// The "help me file a bug" block: copy a version+config header + the log tail (capped), or
	// reveal the log folder in the OS file manager. Both must not panic on a first launch with no
	// log dir — the backend creates/handles that.
	let copyingDiagnostics = $state(false);
	let revealingLogs = $state(false);

	// QURATOR-68 — the NAT classification reading. 'undetermined' is the explicit "not yet
	// determined" state (probe not yet complete / cold or offline start), NEVER a confident "no NAT"
	// — the same rule as QURATOR-67 / QURATOR-80's empty-state fix: unknown must not render as a
	// confident negative. CGNAT is "strong signal, not proof" — the copy must not overclaim.
	let natClass = $state<NatClassification>('undetermined');
	function natLabelFor(c: NatClassification): string {
		switch (c) {
			case 'cgnat': return 'CGNAT (carrier-grade NAT) detected';
			case 'nat': return 'Behind NAT';
			case 'no-nat': return 'No NAT';
			case 'unknown': return 'Unknown';
			default: return 'Not yet determined';
		}
	}
	function natSubFor(c: NatClassification): string {
		switch (c) {
			case 'cgnat':
				return 'Your public address is in the range providers use for carrier-grade NAT. A strong sign, not proof.';
			case 'nat':
				return 'Your local address is a private one, or it differs from the address the outside world sees.';
			case 'no-nat':
				return 'Your local address is what the outside world sees.';
			case 'unknown':
				return 'No mapped address was seen and your local address is not a private one. The result is undecided. It does not mean no NAT.';
			default:
				return 'Still checking. Reopen Settings in a moment.';
		}
	}
	let natLabel = $derived(natLabelFor(natClass));
	let natSub = $derived(natSubFor(natClass));

	async function handleCopyDiagnostics() {
		copyingDiagnostics = true;
		try {
			const text = await copyDiagnostics();
			await handleCopy(text);
			toast('Diagnostics copied. Paste it into your bug report.', 'success');
		} catch (e) { toast(String(e), 'error'); }
		finally { copyingDiagnostics = false; }
	}

	async function handleRevealLogs() {
		revealingLogs = true;
		try {
			await revealLogFolder();
		} catch (e) { toast(String(e), 'error'); }
		finally { revealingLogs = false; }
	}

	let relayUrls: string[] = $state([]);
	let newRelay = $state('');
	let savingRelays = $state(false);
	let addingRelay = $state(false);

	type RelayStatus = 'checking' | 'ok' | 'error';
	let relayStatuses: Record<string, RelayStatus> = $state({});
	// devtest #9: per-relay beacon-publish evidence, so a same-NAT reject isn't invisible below debug.
	let beaconReport: BeaconReport | null = $state(null);
	// QURATOR-93: the settings load used to fail into "proceed with defaults", which rendered a
	// DEFAULTS editor with a live Save — one click away from persisting a defaults-shaped settings
	// object over the user's real one. `settingsLoaded` stays false until a load SUCCEEDS; the
	// relay editor's Save is disabled until then, and an error + Retry surfaces instead.
	let settingsLoadFailed = $state(false);
	let settingsLoaded = $state(false);

	/** Load (or re-load) settings. On success populates the editor, probes relays and starts the
	 *  live-status overlay — exactly the onMount body, reused by the Retry affordance. */
	async function loadSettings() {
		settingsLoadFailed = false;
		try {
			settings = await getSettings();
			// Fresh install has no saved relays — show the curated public defaults (the backend
			// falls back to the same set). The user can edit or remove them.
			relayUrls = settings.relay_urls.length ? settings.relay_urls : [...DEFAULT_RELAYS];
			relayUrls.forEach(probeRelay);
			// Overlay the live data-path status once the persistent client has had a moment to dial,
			// then keep it current on a slow tick while the page is open.
			refreshLiveRelayStatus();
			if (!liveStatusTimer) liveStatusTimer = setInterval(refreshLiveRelayStatus, 12_000);
			settingsLoaded = true;
		} catch {
			settingsLoadFailed = true;
		}
	}

	async function probeRelay(url: string) {
		relayStatuses[url] = 'checking';
		relayStatuses = relayStatuses;
		try {
			await checkRelay(url);
			relayStatuses[url] = 'ok';
		} catch {
			relayStatuses[url] = 'error';
		}
		relayStatuses = relayStatuses;
	}

	// M12 W1 Decision D: overlay the **live data-path** status (what the persistent shared client
	// actually sees per relay) onto the rows — not just the on-demand handshake probe. So a relay
	// that the client is rate-limited on / can't keep open reads as Unreachable here, explaining a
	// "–" online chip.
	let liveStatusTimer: ReturnType<typeof setInterval> | undefined;
	async function refreshLiveRelayStatus() {
		try {
			const health = await relayStatus();
			for (const h of health) {
				relayStatuses[h.url] = h.connected
					? 'ok'
					: ['connecting', 'pending', 'initialized'].includes(h.status)
						? 'checking'
						: 'error';
			}
			relayStatuses = relayStatuses;
		} catch { /* keep the probe results */ }
		try {
			beaconReport = await beaconStatus();
		} catch { /* keep the last-known report */ }
	}
	onDestroy(() => { if (liveStatusTimer) clearInterval(liveStatusTimer); });

	let allowDms = $derived(settings.allow_dms);

	let wipeConfirm = $state(false);
	let wiping = $state(false);

	// ── QURATOR-94 — the blocklist surface (dm_unblock previously had NO UI anywhere) ──────────
	let blocked: string[] = $state([]);
	let unblockingNpub: string | null = $state(null);
	// minor-5: a swallowed dmBlockedList() failure used to fall straight into the quiet "No blocked
	// contacts" line, hiding an UNKNOWN blocklist (and every Unblock control) behind a confident
	// negative. Mirror the QURATOR-93 settings-load shape: an explicit failed flag drives an error +
	// Retry instead, and a success — first-try or retry — always clears it (both directions). The
	// list itself is never cleared on failure, so a retry that fails after a prior success still
	// shows the stale rows rather than reverting to the error/empty state.
	let blockedLoadFailed = $state(false);

	async function loadBlocked() {
		try {
			blocked = await dmBlockedList();
			blockedLoadFailed = false;
		} catch {
			blockedLoadFailed = true;
		}
	}

	async function handleUnblock(npub: string) {
		if (unblockingNpub) return;
		unblockingNpub = npub;
		try {
			await dmUnblock(npub);
			await loadBlocked();
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			unblockingNpub = null;
		}
	}

	// ── QURATOR-141 — proactive block-by-npub. Previously dmBlock's ONLY call site was chat's
	// handleBlock(r: DmRequestView), so the blocklist was a surface you could only ever SHRINK —
	// growing it required the stranger to message you first. Validation reuses
	// validateShareCode (ShareCode::parse, bech32-checksummed) rather than a new parser: a
	// typo'd block is silent and useless. A full hbk1 share code is accepted too — the block
	// target is its embedded identity npub, so no contact record is created (out of scope by
	// owner ruling: what blocking a contact means beyond DMs is undecided).
	let blockNpubInput = $state('');
	let blockNpubBusy = $state(false);

	async function handleBlockNpub() {
		const raw = blockNpubInput.trim();
		if (!raw || blockNpubBusy) return;
		blockNpubBusy = true;
		try {
			// The npub is validated BEFORE storing (validate_share_code is local: parse +
			// bech32 checksum, zero network). Accept a full share code by canonicalising to the
			// npub it carries, so pasting someone's hbk1… doesn't block a string that will never
			// match a decoded DM sender.
			const valid = await validateShareCode(raw);
			if (!valid) {
				toast('That is not a valid npub (or share code). Nothing was blocked.', 'error');
				blockNpubBusy = false;
				return;
			}
			const npub = raw.startsWith('npub1') ? raw : await shareCodeInfo(raw).then((i) => i.npub);
			if (npub === ($identity?.npub ?? '')) {
				toast('That is your own npub.', 'error');
				blockNpubBusy = false;
				return;
			}
			await dmBlock(npub);
			blockNpubInput = '';
			await loadBlocked();
			toast('Blocked', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			blockNpubBusy = false;
		}
	}

	onMount(async () => {
		try { appVersion = await getVersion(); } catch { appVersion = ''; }
		loadSettings();
		// Route the updater UI: the portable (loose-exe) build self-replaces; an NSIS install uses the
		// staged/deferred flow. Best-effort — a detection failure falls back to the NSIS path.
		try { isPortable = await updaterIsPortable(); } catch { isPortable = false; }
		// Visible-after "now running vX.Y" notice — fires once per version change.
		try {
			const notice = updateNoticeVM(await takeUpdateNotice());
			if (notice.show) toast(`Now running v${notice.version}.`, 'success');
		} catch { /* updater not configured */ }
		// QURATOR-68: read the NAT classification the startup probe wrote. Best-effort — a failure
		// leaves the explicit "Not yet determined" state, never a confident negative. Re-checked
		// after a short delay so a slow probe (cold relay discovery) still surfaces a real reading.
		try { natClass = await natClassification(); } catch { natClass = 'undetermined'; }
		loadBlocked();
		if (natClass === 'undetermined') {
			setTimeout(async () => {
				try { natClass = await natClassification(); } catch { /* keep undetermined */ }
			}, 6_000);
		}
	});

	async function handleGenerate() {
		generating = true;
		try {
			const info = await generateKeypair();
			identity.set(info);
			toast('Keypair generated');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			generating = false;
		}
	}

	async function handleCopy(text: string) {
		try {
			// Try the modern clipboard API first; fall back to execCommand for
			// environments where navigator.clipboard is restricted.
			try {
				await navigator.clipboard.writeText(text);
			} catch {
				const el = document.createElement('textarea');
				el.value = text;
				el.style.cssText = 'position:fixed;opacity:0;pointer-events:none';
				document.body.appendChild(el);
				el.select();
				document.execCommand('copy');
				document.body.removeChild(el);
			}
			copied = true;
			setTimeout(() => (copied = false), 2000);
		} catch {
			toast('Could not copy to clipboard', 'error');
		}
	}

	// Merge live relay edits into the preserved settings object so save never drops a field.
	function fullSettings(): Settings {
		return { ...settings, relay_urls: relayUrls };
	}

	// M16 W3: persist the optional big relay (empty = feature off). Trimmed on save so a stray space
	// isn't stored (mirrors the backend's trim in save_settings).
	let savingBigRelay = $state(false);
	async function handleSaveBigRelay() {
		// QURATOR-93 twin of the relays guard (the hardened-path drift pair rule).
		if (!settingsLoaded) return;
		savingBigRelay = true;
		try {
			settings = { ...settings, big_relay_url: settings.big_relay_url.trim() };
			await saveSettings(fullSettings());
			toast('Big relay saved', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			savingBigRelay = false;
		}
	}

	async function toggleAllowDms() {
		// QURATOR-93 twin (M1): the toggles bypassed the settingsLoaded gate — belt-and-suspenders
		// with the disabled attribute below, since a click that lands before Svelte re-renders the
		// disabled state must not be allowed to fire a save of the fallback object.
		if (!settingsLoaded) return;
		settings = { ...settings, allow_dms: !settings.allow_dms };
		try {
			await saveSettings(fullSettings());
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// M9 reactive mirrors of the snapshot toggles (preserved through full-object saves).
	let snapshotAutoUpdate = $derived(settings.snapshot_auto_update);
	let snapshotReconcilePoll = $derived(settings.snapshot_reconcile_poll);

	// Toggle one boolean field and persist the whole object (never drop another field — the M5
	// fullSettings() gotcha).
	async function toggleSetting(field: 'snapshot_auto_update' | 'snapshot_reconcile_poll') {
		// QURATOR-93 twin (M1): see toggleAllowDms.
		if (!settingsLoaded) return;
		settings = { ...settings, [field]: !settings[field] };
		try {
			await saveSettings(fullSettings());
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	// devtest #5: the discoverability opt-out. Flip + persist like the other toggles, then — only if
	// a teaser is already published — republish it so the change (add/drop hashtags) takes effect
	// immediately instead of waiting for the next unrelated publish.
	async function toggleDiscoverable() {
		// QURATOR-93 twin (M1): see toggleAllowDms.
		if (!settingsLoaded) return;
		settings = { ...settings, discoverable: !settings.discoverable };
		try {
			await saveSettings(fullSettings());
			if (await hasPublishedProfile()) await publishProfile();
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleWipe() {
		wiping = true;
		try {
			await wipeData();
			toast('Data wiped. Published data may linger on the relay for up to 24 hours. Restarting…');
			await new Promise(r => setTimeout(r, 3000));
			await relaunch();
		} catch (e) {
			toast(String(e), 'error');
			wiping = false;
		}
	}

	async function addRelay() {
		const check = validateRelayUrl(newRelay);
		if (!check.ok) { toast(check.error, 'error'); return; }
		const url = check.url;
		if (relayUrls.includes(url)) return;
		addingRelay = true;
		try {
			await checkRelay(url);
		} catch (e) {
			toast(`Could not connect to relay: ${String(e)}`, 'error');
			addingRelay = false;
			return;
		}
		relayUrls = [...relayUrls, url];
		relayStatuses[url] = 'ok';
		relayStatuses = relayStatuses;
		newRelay = '';
		addingRelay = false;
	}

	function removeRelay(url: string) {
		relayUrls = relayUrls.filter((u) => u !== url);
		const { [url]: _, ...rest } = relayStatuses;
		relayStatuses = rest;
	}

	async function handleSaveRelays() {
		// QURATOR-93: the disabled attribute is the UI gate; this guard is the enforcement. A save
		// from an unloaded editor persists a defaults-shaped object over the user's real settings.
		if (!settingsLoaded) return;
		savingRelays = true;
		try {
			settings = fullSettings();
			await saveSettings(settings);
			toast('Relay settings saved');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			savingRelays = false;
		}
	}

	let idName = $derived($profile?.display_name ?? 'You');
	let idInitial = $derived(idName[0]?.toUpperCase() ?? 'Y');
	let idHue = $derived(avatarHue(idInitial));

	// Profile picture changing lives on Home (devtest #5) — Settings only displays the avatar.

	function relayDotColor(status: RelayStatus | undefined) {
		if (status === 'ok') return 'var(--online)';
		if (status === 'error') return 'var(--error)';
		return 'var(--fg-dim)'; // checking or unknown
	}

	function relayStatusLabel(status: RelayStatus | undefined) {
		if (status === 'ok') return 'Connected';
		if (status === 'error') return 'Unreachable';
		if (status === 'checking') return 'Checking…';
		return 'Not checked';
	}
</script>

<!-- TopBar -->
<!-- QURATOR-81 follow-up — see contacts/+page.svelte: the topbar is the drag handle. The attribute
     does not inherit, so the controls inside keep their clicks. -->
<div class="topbar" data-tauri-drag-region>
	<div>
		<div class="topbar-title">Settings</div>
		<div class="topbar-sub">Identity, relays, and preferences</div>
	</div>
</div>

<div class="body">
	<!-- Identity -->
	<div class="section-label">Identity</div>

	{#if $identity && kv}
		<div class="surface">
			<div class="identity-top">
				<Avatar letter={idInitial} size={56} hue={idHue} picture={$profile?.picture} />
				<div class="identity-info">
					<div class="identity-name">{idName}</div>
				</div>
				</div>

			<!-- The three keys: npub (irreplaceable), iroh node key (public), share code (carries the browse-key). -->
			{#each kv.rows as row (row.label)}
				<div class="field-label" style="margin-bottom:4px">{row.label}{#if row.sensitive} <span class="key-secret">secret</span>{/if}</div>
				<div class="id-display">
					<span class="id-text">{row.value}</span>
					<button class="icon-btn" onclick={() => handleCopy(row.value)} title={row.label === 'Share code' ? 'Copy share code' : 'Copy npub'}>{@html icons.copy}</button>
				</div>
				{#if row.hint}<div class="id-hint" style="margin-bottom:12px">{row.hint}</div>{/if}
			{/each}

			{#if copied}
				<div class="id-actions"><span class="id-hint">Copied!</span></div>
			{/if}

			<div class="no-recovery">{@html icons.key} {kv.noRecoveryNotice}</div>

			{#if kv.showStorageWarning}
				<div class="key-storage-warn">
					{@html icons.key} Your key is stored as a protected file, not in an OS keyring on this
					platform. Anyone with access to your user account can read it, so keep this device secure.
				</div>
			{/if}
		</div>

		<!-- Backup / restore -->
		<div class="section-label">Backup &amp; restore</div>
		<div class="surface">
			<div class="field-label">Export a portable backup of everything: your keys, collections, contacts, and settings. Store it somewhere safe. It is your only protection against losing your identity.</div>
			<div class="backup-modes">
				{#each backupModes as opt (opt.mode)}
					<label class="backup-mode" class:backup-mode-on={backupMode === opt.mode}>
						<input type="radio" name="backupMode" value={opt.mode} bind:group={backupMode} />
						<div>
							<div class="backup-mode-label">{opt.label}{#if opt.warned} ⚠{/if}</div>
							<div class="toggle-sub">{opt.description}</div>
						</div>
					</label>
				{/each}
			</div>
			{#if backupMode === 'passphrase'}
				<input class="hb-input" type="password" placeholder="Backup passphrase (min 12 characters)" bind:value={backupPass} />
				{#if backupPass}
					<div class="strength-row">
						<div class="strength-bar"><div class="strength-fill" style="width:{backupStrength.score * 25}%" class:strength-bad={!backupStrength.acceptable}></div></div>
						<span class="strength-label">{backupStrength.label}</span>
					</div>
					{#if backupStrength.reason}<div class="toggle-sub">{backupStrength.reason}</div>{/if}
				{/if}
			{/if}
			<div style="display:flex; gap:8px; flex-wrap:wrap;">
				<button class="btn-primary btn-sm" onclick={handleBackup} disabled={backingUp || (backupMode === 'passphrase' && !backupStrength.acceptable)}>
					{backingUp ? 'Exporting…' : 'Export backup'}
				</button>
				<button class="btn-default btn-sm" onclick={pickRestore} disabled={restoring}>
					{@html icons.key} {restoring ? 'Restoring…' : 'Restore from backup'}
				</button>
			</div>
			{#if restoreNeedsPass && restorePath}
				<div class="restore-pass">
					<input class="hb-input" type="password" placeholder="Backup passphrase" bind:value={restorePass} disabled={restoring} />
					<button class="btn-primary btn-sm" onclick={doRestore} disabled={!restorePass || restoring}>Restore</button>
					<button class="btn-ghost btn-sm" onclick={() => { restorePath = null; restoreNeedsPass = false; }} disabled={restoring}>Cancel</button>
				</div>
			{/if}
		</div>

		<!-- Import a different key -->
		<div class="section-label">Use a different Nostr key</div>
		<div class="surface">
			{#if !importOpen}
				<div class="toggle-row">
					<div class="toggle-text">
						<div class="toggle-label">Import an existing Nostr key</div>
						<div class="toggle-sub">Replaces this identity. Wipe data first if you already have one.</div>
					</div>
					<button class="btn-default btn-sm" onclick={() => (importOpen = true)}>Import nsec</button>
				</div>
			{:else}
				<div class="link-warn">
					{@html icons.key} <strong>Linking warning:</strong> importing a key you use publicly, in
					Qurator, or anywhere else ties that identity to your Hoardbook activity. Anyone who knows
					the key will know this is you.
				</div>
				<label class="ack-row"><input type="checkbox" bind:checked={importWarnAck} /> I understand.</label>
				<input class="hb-input hb-mono" type="password" placeholder="nsec1…" bind:value={importNsecValue} />
				<div style="display:flex; gap:8px;">
					<button class="btn-primary btn-sm" onclick={handleImportNsec} disabled={!importWarnAck || !importNsecValue.trim() || importingNsec}>
						{importingNsec ? 'Importing…' : 'Import key'}
					</button>
					<button class="btn-ghost btn-sm" onclick={() => { importOpen = false; importNsecValue = ''; importWarnAck = false; }}>Cancel</button>
				</div>
			{/if}
		</div>
	{:else}
		<div class="surface">
			<p class="no-id-text">No identity yet. Generate one, or restore from a backup.</p>
			<div style="display:flex; gap:8px; flex-wrap:wrap;">
				<button class="btn-primary" onclick={handleGenerate} disabled={generating}>
					{generating ? 'Generating…' : 'Generate identity'}
				</button>
				<button class="btn-default" onclick={pickRestore} disabled={restoring}>
					{@html icons.key} {restoring ? 'Restoring…' : 'Restore from backup'}
				</button>
			</div>
			{#if restoreNeedsPass && restorePath}
				<div class="restore-pass">
					<input class="hb-input" type="password" placeholder="Backup passphrase" bind:value={restorePass} disabled={restoring} />
					<button class="btn-primary btn-sm" onclick={doRestore} disabled={!restorePass || restoring}>Restore</button>
					<button class="btn-ghost btn-sm" onclick={() => { restorePath = null; restoreNeedsPass = false; }} disabled={restoring}>Cancel</button>
				</div>
			{/if}
		</div>
	{/if}

	<!-- Relays -->
	<div class="section-row">
		<div class="section-label">Relays<FeatureTooltip key="custom-relays" /></div>
	</div>

	<div class="surface surface-nop">
		{#if settingsLoadFailed}
			<!-- QURATOR-93: the failed settings load used to fall through to a DEFAULTS editor with a
			     live Save — one click from persisting defaults over the user's real settings. Show the
			     error + Retry instead, and keep Save disabled until a load succeeds (see below). -->
			<EmptyState
				error
				message="Couldn't load your settings. Saving is disabled until they load."
				onretry={loadSettings}
			/>
		{/if}
		{#each relayUrls as url (url)}
			{@const status = relayStatuses[url]}
			{@const bv = beaconLine(beaconReport, url, Date.now() / 1000)}
			<div class="relay-row">
				<div class="relay-dot" style="background:{relayDotColor(status)}" class:relay-dot-pulse={status === 'checking'}></div>
				<div class="relay-info">
					<div class="relay-url">{url}</div>
					<div class="relay-meta">
						<span class:status-ok={status === 'ok'} class:status-err={status === 'error'}>{relayStatusLabel(status)}</span>
						<span class="beacon-line" class:status-ok={bv.tone === 'ok'} class:status-err={bv.tone === 'bad'} class:status-warn={bv.tone === 'warn'}>{bv.text}</span>
					</div>
				</div>
				<button class="icon-btn" title="Re-check" onclick={() => probeRelay(url)}>{@html icons.refresh}</button>
				<button class="icon-btn" onclick={() => removeRelay(url)}>{@html icons.close}</button>
			</div>
		{/each}
		<!-- v0.12.10 diagnostic: loop-liveness breadcrumb (rendered once, not per-relay) — the
		     shipped Windows build has no log subscriber, so this line is the on-screen evidence the
		     presence task is being polled at all. -->
		<div class="relay-loop-line">{loopLine(beaconReport)}</div>
		<!-- Add relay row -->
		<div class="relay-add-row">
			<input
				class="hb-input hb-mono"
				type="text"
				placeholder="wss://relay.example.com"
				bind:value={newRelay}
				onkeydown={(e) => e.key === 'Enter' && addRelay()}
			/>
			<button class="btn-default btn-sm" onclick={addRelay} disabled={!newRelay.trim() || addingRelay}>
				{addingRelay ? 'Checking…' : 'Add'}
			</button>
			<button class="btn-primary btn-sm" onclick={handleSaveRelays} disabled={savingRelays || !settingsLoaded}>
				{savingRelays ? 'Saving…' : 'Save'}
			</button>
		</div>
	</div>

<!-- ===================================================================================
     BIG RELAY — TEMPORARILY DISABLED, owner 2026-08-27. Commented out rather than deleted so it
     can come back unchanged. The BACKEND IS UNTOUCHED: `settings.big_relay_url` still round-trips
     through get/saveSettings, M16 Layer 3 still publishes there when the value is non-empty, and
     `handleSaveBigRelay` is still defined above. Only the way to EDIT it is hidden.
     ⚠ Consequence to know: anyone who already set a big relay keeps using it with no way to see or
     clear it from the UI. If that is not wanted, the value must also be cleared on upgrade — that
     is a separate decision, deliberately NOT taken here.
     To restore: delete this comment opener and its closer below.

     (was: M16 W3 - optional big relay for large-collection full manifests)
	<div class="section-label">Big relay (large collections)</div>
	<div class="surface surface-nop">
		<div class="relay-add-row">
			<input
				class="hb-input hb-mono"
				type="text"
				placeholder="ws://your-big-relay.example:7777 (optional)"
				bind:value={settings.big_relay_url}
				onkeydown={(e) => e.key === 'Enter' && handleSaveBigRelay()}
			/>
			<button class="btn-primary btn-sm" onclick={handleSaveBigRelay} disabled={savingBigRelay || !settingsLoaded}>
				{savingBigRelay ? 'Saving…' : 'Save'}
			</button>
		</div>
		<div class="relay-hint">
			A higher-capacity relay you run for collections too large to publish whole. When set,
			publishing a large collection also sends its full listing here; people with your share code
			fetch the rest from it. Leave empty to publish only the preview.
		</div>
	</div>
     =================================================================================== -->

	<!-- Preferences -->
	<div class="section-label">Preferences</div>

	<div class="surface">
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Allow incoming messages from anyone</div>
				<div class="toggle-sub">Off means only your contacts can DM you</div>
			</div>
			<button class="toggle" class:toggle-on={allowDms} onclick={toggleAllowDms} disabled={!settingsLoaded} aria-label="Allow incoming messages from anyone">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Auto-update snapshots on change</div>
				<div class="toggle-sub">
					Re-publish a published collection when its folder changes. When off, only a manual rescan
					updates it. Changes made from another computer on a network share are picked up at launch.
				</div>
			</div>
			<button class="toggle" class:toggle-on={snapshotAutoUpdate} onclick={() => toggleSetting('snapshot_auto_update')} disabled={!settingsLoaded} aria-label="Auto-update snapshots on change">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Reconcile poll for remotely-edited collections</div>
				<div class="toggle-sub">Low-frequency re-check for collections you edit from another host (SMB). Off by default.</div>
			</div>
			<button class="toggle" class:toggle-on={snapshotReconcilePoll} onclick={() => toggleSetting('snapshot_reconcile_poll')} disabled={!settingsLoaded} aria-label="Reconcile poll for remotely-edited collections">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Show up in Discover Hoarders</div>
				<div class="toggle-sub">
					Off means people can't find you by tag or content-type search. They can still reach you
					with your npub or share code, and your contacts are unaffected.
				</div>
			</div>
			<button class="toggle" class:toggle-on={settings.discoverable} onclick={toggleDiscoverable} disabled={!settingsLoaded} aria-label="Show up in Discover Hoarders">
				<span class="toggle-thumb"></span>
			</button>
		</div>
	</div>

	<!-- Blocked contacts (QURATOR-94 — the unblock surface; previously unreachable from any UI).
	     QURATOR-141 adds the proactive block input below — the only other way to GROW the list was
	     chat's handleBlock, which needs an existing DM request. -->
	<div class="section-label">Blocked contacts</div>
	<div class="surface surface-nop">
		<div class="relay-add-row blocked-add-row">
			<input
				class="hb-input hb-mono"
				type="text"
				placeholder="npub1… (paste an npub to block them)"
				bind:value={blockNpubInput}
				onkeydown={(e) => e.key === 'Enter' && handleBlockNpub()}
				disabled={blockNpubBusy}
			/>
			<button class="btn-default btn-sm" onclick={handleBlockNpub} disabled={!blockNpubInput.trim() || blockNpubBusy}>
				{blockNpubBusy ? 'Blocking…' : 'Block'}
			</button>
		</div>
		{#if blockedLoadFailed && blocked.length === 0}
			<!-- minor-5: an unknown blocklist must not render as the confident "No blocked contacts"
			     line — same rule as QURATOR-93/QURATOR-80/85. -->
			<EmptyState
				error
				message="Couldn't load your blocked contacts."
				onretry={loadBlocked}
			/>
		{:else if blocked.length === 0}
			<div class="blocked-empty">No blocked contacts.</div>
		{:else}
			{#each blocked as npub (npub)}
				<div class="blocked-row">
					<div class="relay-info">
						<div class="relay-url mono">{shortNpub(npub)}</div>
						<div class="relay-meta"><span>Can't message you; their requests are dropped.</span></div>
					</div>
					<button class="btn-ghost btn-sm" disabled={unblockingNpub === npub} onclick={() => handleUnblock(npub)}>
						{unblockingNpub === npub ? '…' : 'Unblock'}
					</button>
				</div>
			{/each}
		{/if}
	</div>


	<!-- Updates -->
	<div class="section-label">Updates</div>
	<div class="surface">
		<div class="update-row">
			<div class="toggle-text">
				<div class="toggle-label">App version</div>
				<div class="toggle-sub">Currently running v{appVersion || '…'}</div>
			</div>
			<div class="update-actions">
				{#if isPortable}
					<!-- Portable build: one-step download → verify → self-replace → restart. -->
					{#if portableInfo}
						<span class="update-available-text">v{portableInfo.version} available</span>
						<button class="btn-primary btn-sm" onclick={doApplyPortable} disabled={portableApplying}>
							{portableApplying ? 'Updating…' : 'Update & restart'}
						</button>
					{:else if updateChecked}
						<span class="update-ok-text">Up to date</span>
					{/if}
				{:else if stagedVersion}
					<span class="update-available-text">v{stagedVersion} downloaded</span>
					<button class="btn-primary btn-sm" onclick={doApplyUpdate}>Restart &amp; apply</button>
				{:else if updateInfo}
					<span class="update-available-text">v{updateInfo.version} available</span>
					<button class="btn-primary btn-sm" onclick={doDownloadUpdate} disabled={updateStaging}>
						{updateStaging ? 'Downloading…' : 'Download update'}
					</button>
				{:else if updateChecked}
					<span class="update-ok-text">Up to date</span>
				{/if}
				<button class="btn-default btn-sm" onclick={doCheckUpdate} disabled={updateChecking}>
					{updateChecking ? 'Checking…' : 'Check for updates'}
				</button>
			</div>
		</div>
		{#if isPortable && portableInfo}
			<div class="toggle-sub">Downloads, verifies (minisign), and replaces this portable app in place, then restarts.</div>
		{:else if stagedVersion}
			<div class="toggle-sub">Downloaded and verified. It installs automatically when you quit Hoardbook (or click "Restart &amp; apply").</div>
		{/if}
		{#if updateError}
			<div class="update-error-text">{updateError}</div>
		{/if}
	</div>

	<!-- Diagnostics (QURATOR-65): the "help me file a bug" block. Copy a version+config header +
	     capped log tail, or reveal the log folder. -->
	<div class="section-label">Diagnostics</div>
	<div class="surface">
		<!-- QURATOR-68: the NAT classification row. One line + sub-label, no address data (INV).
		     'undetermined' / 'unknown' are explicit not-yet-decided surfaces, never a confident
		     "no NAT" — the same rule as QURATOR-67 / QURATOR-80's empty-state fix. CGNAT is
		     "strong signal, not proof" and the copy must not overclaim. -->
		<div class="nat-row" data-nat-class={natClass}>
			<div class="toggle-text">
				<div class="toggle-label">Network type<FeatureTooltip key="network-type" /></div>
				<div class="toggle-sub">{natSub}</div>
			</div>
			<div
				class="nat-pill"
				class:nat-cgnat={natClass === 'cgnat'}
				class:nat-nat={natClass === 'nat'}
				class:nat-no={natClass === 'no-nat'}
				class:nat-unknown={natClass === 'unknown' || natClass === 'undetermined'}
			>{natLabel}</div>
		</div>
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Copy diagnostics to clipboard</div>
				<div class="toggle-sub">
					A version and config header (npub truncated, no keys) plus the recent log, ready to paste
					into a bug report or Reddit comment.
				</div>
			</div>
			<button class="btn-default btn-sm" onclick={handleCopyDiagnostics} disabled={copyingDiagnostics}>
				{copyingDiagnostics ? 'Copying…' : 'Copy diagnostics'}
			</button>
		</div>
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Reveal log folder</div>
				<div class="toggle-sub">Open the OS file manager at the daily-rotated log files.</div>
			</div>
			<button class="btn-default btn-sm" onclick={handleRevealLogs} disabled={revealingLogs}>
				{@html icons.folder} {revealingLogs ? 'Opening…' : 'Reveal logs'}
			</button>
		</div>
	</div>

	<!-- Watches section removed 2026-08-03: the backend (watches_get/create/delete/evaluate) exists
	     but no UI ever creates a watch — the save-search affordance in Discover was never built, so
	     the section was a permanently-empty list. Restore it when Discover grows "save this search". -->

	<!-- Danger Zone -->
	<div class="section-label danger-label">Danger zone</div>

	<div class="surface danger-surface">
		<div class="danger-row">
			<div>
				<div class="toggle-label">Wipe all data</div>
				<div class="toggle-sub">Permanently removes your identity, profile, and app data from this device. Your files on disk are not touched. Only Hoardbook's database is cleared.</div>
			</div>
			{#if !wipeConfirm}
				<button class="btn-danger btn-sm" onclick={() => (wipeConfirm = true)}>Wipe data</button>
			{:else}
				<div class="wipe-confirm">
					<span class="wipe-warn">Are you sure? This is permanent.</span>
					<button class="btn-danger btn-sm" onclick={handleWipe} disabled={wiping}>
						{wiping ? 'Wiping…' : 'Confirm wipe'}
					</button>
					<button class="btn-ghost btn-sm" onclick={() => (wipeConfirm = false)}>Cancel</button>
				</div>
			{/if}
		</div>
	</div>
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

	.body { padding: 24px; overflow-y: auto; flex: 1; max-width: 720px; display: flex; flex-direction: column; gap: 8px; }

	.section-label {
		font-size: 10.5px;
		color: var(--fg-dim);
		text-transform: uppercase;
		letter-spacing: 1.2px;
		font-weight: 600;
		padding-top: 16px;
	}

	.danger-label { color: var(--error); }

	.section-row { display: flex; justify-content: space-between; align-items: center; padding-top: 16px; }

	.surface {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 18px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.surface-nop { padding: 0; gap: 0; }

	.danger-surface { border-color: color-mix(in oklch, var(--error) 30%, transparent); }

	.identity-top { display: flex; gap: 16px; align-items: center; }

	.identity-info { flex: 1; }

	.identity-name { font-size: 14px; font-weight: 600; }

	.key-storage-warn {
		margin-top: 12px;
		padding: 10px 12px;
		border: 1px solid color-mix(in oklch, var(--accent) 35%, transparent);
		background: color-mix(in oklch, var(--accent) 8%, transparent);
		border-radius: 7px;
		font-size: 12px;
		color: var(--fg-muted);
		display: flex;
		gap: 8px;
		align-items: flex-start;
		line-height: 1.5;
	}

	.id-display {
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 10px 12px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg);
		display: flex;
		align-items: center;
		gap: 10px;
		word-break: break-all;
	}

	.id-text { flex: 1; }

	.id-actions {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 12px;
	}

	.id-hint { font-size: 11.5px; color: var(--fg-dim); }

	.no-id-text { font-size: 13px; color: var(--fg-muted); }

	.field-label { font-size: 11px; color: var(--fg-muted); font-weight: 500; }

	/* Blocked-contact rows (QURATOR-94) — same shape as the relay rows above */
	.blocked-row {
		padding: 12px 16px;
		display: flex;
		gap: 14px;
		align-items: center;
		justify-content: space-between;
		border-bottom: 1px solid var(--divider);
	}
	.blocked-row:last-child { border-bottom: none; }
	.blocked-empty {
		padding: 12px 16px;
		font-size: 12px;
		color: var(--fg-muted);
	}

	/* Relay rows */
	.relay-row {
		padding: 12px 16px;
		display: flex;
		gap: 14px;
		align-items: center;
		border-bottom: 1px solid var(--divider);
	}

	.relay-dot {
		width: 8px; height: 8px;
		border-radius: 50%;
		flex-shrink: 0;
	}

	.relay-info { flex: 1; min-width: 0; }

	.relay-url {
		font-family: var(--font-mono);
		font-size: 12.5px;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.relay-meta {
		display: flex;
		gap: 8px;
		font-size: 11px;
		color: var(--fg-dim);
		margin-top: 2px;
	}

	.status-ok  { color: var(--online); }
	.status-err { color: var(--error); }
	.status-warn { color: var(--fg-muted); }

	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50%       { opacity: 0.3; }
	}
	.relay-dot-pulse { animation: pulse 1s ease-in-out infinite; }

	.relay-add-row {
		padding: 12px 16px;
		display: flex;
		gap: 8px;
		align-items: center;
	}

	/* M16 W3: help text under the big-relay input (surface-nop has no padding of its own). */
	.relay-hint {
		padding: 0 16px 12px;
		font-size: 11px;
		color: var(--fg-dim);
		line-height: 1.4;
	}

	/* v0.12.10 diagnostic: the loop-liveness breadcrumb, once under the relay rows. */
	.relay-loop-line {
		padding: 0 16px 8px;
		font-size: 11px;
		color: var(--fg-muted);
		font-family: var(--mono, monospace);
	}

	/* M20 W4: input contract is global in app.css. Only the flex-grow LAYOUT stays, scoped to the
	   two row containers where the input must grow — never re-declaring .hb-input itself. */
	.relay-add-row > .hb-input,
	.blocked-add-row > .hb-input,
	.restore-pass > .hb-input { flex: 1; }

	/* QURATOR-141: the block-by-npub input row sits inside surface-nop, so it needs the same
	   padding the blocked rows carry; the hairline separates it from the list below. */
	.blocked-add-row { padding: 12px 16px; border-bottom: 1px solid var(--divider); }


	/* Toggles */
	.toggle-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; }

	.toggle-text { flex: 1; }

	.toggle-label { font-size: 12.5px; color: var(--fg); font-weight: 500; }

	.toggle-sub { font-size: 11px; color: var(--fg-dim); margin-top: 1px; }

	/* QURATOR-68 — the NAT classification row. Visually the same shape as a toggle-row but the
	   right-hand slot is a coloured pill, not a button. `nat-unknown` covers both 'undetermined'
	   and 'unknown' (the explicit not-yet-decided surfaces) and uses the muted tone so it reads
	   visually distinct from a confident answer — never as a confident negative. */
	.nat-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 12px;
	}

	.nat-pill {
		flex-shrink: 0;
		font-size: 11px;
		font-weight: 600;
		padding: 4px 10px;
		border-radius: 99px;
		border: 1px solid var(--border);
		background: var(--bg-elev2);
		color: var(--fg-muted);
		white-space: nowrap;
	}
	.nat-pill.nat-cgnat {
		color: var(--accent);
		border-color: color-mix(in oklch, var(--accent) 45%, transparent);
		background: color-mix(in oklch, var(--accent) 12%, transparent);
	}
	.nat-pill.nat-nat {
		color: var(--fg);
		border-color: color-mix(in oklch, var(--fg) 30%, transparent);
	}
	.nat-pill.nat-no {
		color: color-mix(in oklch, var(--accent) 75%, var(--fg) 25%);
		border-color: color-mix(in oklch, var(--accent) 30%, transparent);
	}
	.nat-pill.nat-unknown {
		color: var(--fg-dim);
		font-style: italic;
		font-weight: 500;
	}

	.toggle {
		width: 30px; height: 17px;
		border-radius: 99px;
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		position: relative;
		flex-shrink: 0;
		cursor: pointer;
		transition: background 0.15s, border-color 0.15s;
	}
	.toggle-on { background: var(--accent); border-color: var(--accent); }
	/* M1 (settings-load review): the toggle is a custom control, not on the .btn contract, so
	   disabled needs its own cue — same shape as .btn:disabled in app.css. */
	.toggle:disabled { opacity: 0.5; cursor: not-allowed; }

	.toggle-thumb {
		position: absolute;
		top: 1px; left: 1px;
		width: 13px; height: 13px;
		border-radius: 50%;
		background: var(--fg-muted);
		transition: left 0.15s, background 0.15s;
	}
	.toggle-on .toggle-thumb { left: 14px; background: var(--accent-text); }

	/* Danger zone */
	.danger-row {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 16px;
	}

	.wipe-confirm {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-shrink: 0;
	}

	.wipe-warn {
		font-size: 11.5px;
		color: var(--error);
		white-space: nowrap;
	}

	/* Updates */
	.update-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
	.update-actions { display: flex; gap: 8px; align-items: center; flex-shrink: 0; flex-wrap: wrap; justify-content: flex-end; }
	.update-available-text { font-size: 12px; color: var(--accent); font-weight: 600; white-space: nowrap; }
	.update-ok-text { font-size: 12px; color: var(--online); white-space: nowrap; }
	.update-error-text { font-size: 11.5px; color: var(--error); margin-top: 4px; }

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

	.icon-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-dim);
		display: flex;
		padding: 2px;
	}

	/* Buttons */
	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */
	.btn-default { flex-shrink: 0; min-width: max-content; } /* keep settings-row layout guards only */

	/* M5: 3-key view, backup/restore, import-nsec, share QR */
	.key-secret {
		font-size: 9.5px; text-transform: uppercase; letter-spacing: 0.5px;
		color: var(--error); border: 1px solid color-mix(in oklch, var(--error) 30%, transparent);
		border-radius: 4px; padding: 0 5px; margin-left: 4px;
	}
	.no-recovery {
		margin-top: 4px; padding: 10px 12px; border-radius: 7px;
		border: 1px solid color-mix(in oklch, var(--error) 25%, transparent);
		background: color-mix(in oklch, var(--error) 7%, transparent);
		font-size: 11.5px; color: var(--fg-muted); line-height: 1.5;
		display: flex; gap: 8px; align-items: flex-start;
	}
	.backup-modes { display: flex; flex-direction: column; gap: 8px; }
	.backup-mode {
		display: flex; gap: 10px; align-items: flex-start; padding: 10px 12px;
		border: 1px solid var(--border); border-radius: 8px; cursor: pointer;
	}
	.backup-mode-on { border-color: var(--accent); background: color-mix(in oklch, var(--accent) 7%, transparent); }
	.backup-mode-label { font-size: 12.5px; font-weight: 500; color: var(--fg); }
	.strength-row { display: flex; align-items: center; gap: 10px; }
	.strength-bar { flex: 1; height: 6px; border-radius: 99px; background: var(--bg-elev3); overflow: hidden; }
	.strength-fill { height: 100%; background: var(--online); transition: width 0.15s; }
	.strength-fill.strength-bad { background: var(--error); }
	.strength-label { font-size: 11px; color: var(--fg-dim); white-space: nowrap; }
	.restore-pass { display: flex; gap: 8px; align-items: center; margin-top: 4px; }
	.link-warn {
		padding: 10px 12px; border-radius: 7px;
		border: 1px solid color-mix(in oklch, var(--accent) 35%, transparent);
		background: color-mix(in oklch, var(--accent) 8%, transparent);
		font-size: 11.5px; color: var(--fg-muted); line-height: 1.5;
		display: flex; gap: 8px; align-items: flex-start;
	}
	.ack-row { display: flex; gap: 8px; align-items: center; font-size: 12px; color: var(--fg-muted); }
</style>
