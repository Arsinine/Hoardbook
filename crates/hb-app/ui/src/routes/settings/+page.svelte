<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { generateKeypair, getSettings, saveSettings, backupData, peekBackup, restoreData, validateBackup, wipeData, clearManifestCache, checkRelay, relayStatus, beaconStatus, checkUpdate, downloadUpdate, applyStagedUpdate, takeUpdateNotice, updaterIsPortable, checkPortableUpdate, applyPortableUpdate, hasPublishedProfile, publishProfile, copyDiagnostics, revealLogFolder, natClassification, dmBlockedList, dmUnblock, dmBlock, validateShareCode, shareCodeInfo } from '$lib/api.js';
	import type { Settings, UpdateInfo, PortableUpdateInfo, BeaconReport, NatClassification } from '$lib/api.js';
	import { keyView } from '$lib/key-view.js';
	import { shortNpub } from '$lib/contact-display.js';
	import { passphraseStrength, backupModeOptions, type BackupMode } from '$lib/backup-export.js';
	import { updateNoticeVM } from '$lib/update-ux.js';
	import { beaconLine } from '$lib/beacon-view.js';
	import { effectiveRelays, validateRelayUrl } from '$lib/relays.js';
	import { relaunch } from '@tauri-apps/plugin-process';
	// QURATOR-379: `confirm` is gone from this import — the page asks through the in-app Modal
	// (askInApp below), never a native OS dialog (owner ruling: native popups read as an app
	// fault). `open`/`save` stay: the native file pickers are correct.
	import { open as openFileDialog, save as saveFileDialog } from '@tauri-apps/plugin-dialog';
	import { getVersion } from '@tauri-apps/api/app';
	import { identity, profile, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import FeatureTooltip from '$lib/components/FeatureTooltip.svelte';
	import Modal from '$lib/components/Modal.svelte';
	// QURATOR-352 Appearance: the theme registry and the root-attribute applier.
	import { THEMES, DEFAULT_THEME, BASE_THEME, isThemeId, type ThemeId, type ThemeInfo } from '$lib/themes/themes.js';
	import { applyAppearance } from '$lib/themes/apply.js';

	let generating = $state(false);
	let copied = $state(false);
	let appVersion = $state('');

	// ── QURATOR-379: in-app confirm ──────────────────────────────────────────────
	// The owner ruled notices must never be unprompted native popups (they read as an app fault),
	// and the plugin's `confirm` is exactly that. The page asks through Modal.svelte instead:
	// `askInApp` parks a promise the rendered modal settles. Esc and a backdrop click run Modal's
	// `onclose`, which resolves false — same answer shape the native dialog gave.
	let confirmState = $state<{ title: string; body: string; okLabel: string; danger: boolean } | null>(null);
	let confirmResolve: ((ok: boolean) => void) | null = null;
	function askInApp(title: string, body: string, okLabel: string, danger = true): Promise<boolean> {
		return new Promise((resolve) => {
			confirmState = { title, body, okLabel, danger };
			confirmResolve = resolve;
		});
	}
	function settleConfirm(ok: boolean) {
		confirmState = null;
		confirmResolve?.(ok);
		confirmResolve = null;
	}

	// Full settings object, preserved so saving one field never resets the others (the M5 fields
	// privacy_notice_acknowledged / last_seen_version + the M9 snapshot/online toggles live here too).
	// QURATOR-352: the appearance fields are optional (older settings files lack them; Rust serde
	// defaults fill them — lane D). The Settings interface in lib/api.ts does not carry them yet
	// (the order placed it in types.ts, where no Settings interface exists — reported): this local
	// intersection keeps the page compiling today and is a no-op once the fields land in api.ts.
	type SettingsWithAppearance = Settings & {
		theme?: string;
		glass?: boolean;
	};
	let settings: SettingsWithAppearance = $state({
		relay_urls: [], allow_dms: true, privacy_notice_acknowledged: false,
		last_seen_version: '',
		snapshot_auto_update: true, snapshot_reconcile_poll: false, show_online_count: true,
		discoverable: false,
		big_relay_url: '',
		swarm_caching: false, serving_notice_acknowledged: false,
	});

	// ── QURATOR-352: Appearance section state ────────────────────────────────────
	let themeId = $derived(isThemeId(settings.theme) ? settings.theme : DEFAULT_THEME);
	let glassOn = $derived(settings.glass ?? true);
	// Card subtitle: the default theme (Vault) is labelled "Default"; Hoard — the original palette,
	// no longer the default — is labelled "Original"; every other theme has none. Derived from
	// DEFAULT_THEME / BASE_THEME, never hard-coded. (The design-system numbers the themes were
	// mapped from were dropped 2026-10-13, owner: "These dont mean anything anymore.")
	function themeSubtitle(t: ThemeInfo): string {
		if (t.id === DEFAULT_THEME) return 'Default';
		return t.id === BASE_THEME ? 'Original' : '';
	}

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
			const ok = await askInApp(
				'Plaintext backup',
				'This file is your identity, unencrypted. Anyone who has it can act as you.',
				'Export',
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
		if (!restoreNeedsPass) {
			// QURATOR-275: the header declares plaintext, so no passphrase will ever be asked —
			// make that absence EXPLICIT before anything destructive runs. The header is
			// attacker-controlled: a forged plaintext archive swapped for the user's encrypted one
			// otherwise sails through (prompt skipped, and validateBackup(null)/restoreData(null)
			// is the legitimate plaintext combination, so nothing errors). Informed consent, not a
			// credential: a genuine plaintext backup — which Hoardbook itself produces — stays
			// restorable, it just has to be confirmed, like plaintext export is on the way out.
			const ok = await askInApp(
				'Unencrypted backup',
				'This backup has no passphrase. Anyone with the file could have read or changed it. Restore it anyway?',
				'Restore anyway',
			);
			if (!ok) { restorePath = null; return; }
			doRestore();
		}
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
		const ok = await askInApp(
			'Restore from backup',
			'This replaces all data on this device with the backup, then restarts.',
			'Restore',
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

	// QURATOR-379 (owner, Option A): the Settings Nostr-key import is DELETED. Switching keys is
	// Danger zone → Wipe data → the onboarding wizard's "Use my Nostr key", which already works.
	// The command (`import_nsec`) and the wizard path are unchanged.

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
	// QURATOR-158: the update row itself — the new-version badge routes into it (scrolls it into
	// view), which is the "reuse the existing update UI" half of the ticket.
	let updateRowEl: HTMLElement | undefined = $state();

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

	// QURATOR-158 — the launch-time background check. Deliberately NOT doCheckUpdate(): an
	// unrequested check must fail silently (offline launches are normal), must not claim "up to
	// date" the user never asked to confirm, and must not busy the manual button's spinner. It
	// only fills updateInfo/portableInfo, from which the indicator + the existing update-row UI
	// light up. Must be called AFTER isPortable resolves (onMount does), or a portable build
	// silently takes the NSIS branch.
	async function backgroundUpdateCheck() {
		try {
			if (isPortable) {
				portableInfo = await checkPortableUpdate();
			} else {
				updateInfo = await checkUpdate();
			}
		} catch {
			/* silent by design: an offline/unconfigured updater is not a launch error */
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

	// QURATOR-176 — the manifest-cache clear. One click, the whole cache goes (owner ruling
	// 2026-09-03): NO confirm dialog, NO undo, NO two-step toggle — deliberateness comes from the
	// label, which must say exactly what it does and no more. Copy must not overclaim either: the
	// cache is re-fetchable, so this is disk-space cleanup, not data loss.
	let clearingCache = $state(false);

	async function handleClearManifestCache() {
		if (clearingCache) return;
		clearingCache = true;
		try {
			await clearManifestCache();
			toast('Manifest cache cleared. Cached copies re-download the next time you browse.', 'success');
		} catch (e) { toast(String(e), 'error'); }
		finally { clearingCache = false; }
	}

	// QURATOR-68 — the NAT classification reading. 'undetermined' is the explicit "not yet
	// determined" state (probe not yet complete / cold or offline start), NEVER a confident "no NAT"
	// — the same rule as QURATOR-67 / QURATOR-80's empty-state fix: unknown must not render as a
	// confident negative. CGNAT is "strong signal, not proof" — the copy must not overclaim.
	let natClass = $state<NatClassification>('undetermined');
	function natLabelFor(c: NatClassification): string {
		switch (c) {
			case 'cgnat': return 'CGNAT (carrier-grade NAT) detected';
			case 'nat': return 'NAT';
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
			relayUrls = effectiveRelays(settings.relay_urls);
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
		// QURATOR-158: launch-time check. Strictly AFTER the isPortable line above — the branch
		// pick inside reads it. Never awaited-blocking: mount must not stall on a slow updater.
		void backgroundUpdateCheck();
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

	// QURATOR-164: the one opt-in switch (discovery auto-fetch + relay-caching together). Same
	// flip-and-persist shape as the others — no side effects yet, the behaviour is a later build
	// item that reads this flag.
	async function toggleSwarmCaching() {
		// QURATOR-93 twin (M1): see toggleAllowDms.
		if (!settingsLoaded) return;
		settings = { ...settings, swarm_caching: !settings.swarm_caching };
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

	// ── QURATOR-352: Appearance handlers ─────────────────────────────────────────


	/** Pick a theme card: apply at once, then save. QURATOR-93 twin (M1): never save from the
	 *  fallback object — the in-handler settingsLoaded guard stands even if the DOM gate is
	 *  bypassed, exactly like every other mutator on this page. */
	async function selectTheme(id: ThemeId) {
		if (!settingsLoaded) return;
		if (id === themeId) return; // picking the already-selected card is a no-op (no save, no toast)
		settings.theme = id; // Svelte : mutate the $state proxy in place (never reassign from await)
		applyAppearanceNow();
		try {
			await saveSettings(fullSettings());
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	/** Apply the current appearance to the document root at once. */
	function applyAppearanceNow() {
		applyAppearance({ theme: themeId, glass: glassOn });
	}

	/** Glass effects (default on): apply at once + persist, like every other toggle on this page. */
	async function toggleGlass() {
		// QURATOR-93 twin (M1): see toggleAllowDms.
		if (!settingsLoaded) return;
		settings.glass = !glassOn;
		applyAppearanceNow();
		try {
			await saveSettings(fullSettings());
		} catch (e) {
			toast(String(e), 'error');
		}
	}


	function onThemeGridKeydown(e: KeyboardEvent) {
		const keys = ['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp', 'Home', 'End'];
		if (!keys.includes(e.key)) return;
		e.preventDefault();
		const group = e.currentTarget as HTMLElement;
		const cards = Array.from(group.querySelectorAll<HTMLElement>('[role="radio"]'));
		const active = document.activeElement;
		let i = active ? cards.indexOf(active as HTMLElement) : -1;
		if (i < 0) i = THEMES.findIndex((t) => t.id === themeId);
		if (i < 0) return;
		const dir = (e.key === 'ArrowRight' || e.key === 'ArrowDown') ? 1 : -1;
		const next = e.key === 'Home' ? 0 : e.key === 'End' ? THEMES.length - 1 : (i + dir + THEMES.length) % THEMES.length;
		cards[next].focus();
		selectTheme(THEMES[next].id);
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
	// QURATOR-379: the card's one key line — the truncated npub; the copy button takes the FULL one.
	let idNpubShort = $derived($identity?.npub_short ?? '');
	let idNpub = $derived($identity?.npub ?? '');

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
<div class="topbar hb-pane" data-tauri-drag-region>
	<div>
		<div class="topbar-title">Settings</div>
		<div class="topbar-sub">Identity, relays, and preferences</div>
	</div>
</div>

<div class="body hb-pane">
<div class="body-scroll">
	<!-- Identity -->
	<div class="section-label">Identity</div>

	{#if $identity && kv}
		<div class="surface">
			<div class="identity-top">
				<Avatar letter={idInitial} size={56} hue={idHue} picture={$profile?.picture} />
				<div class="identity-info">
					<div class="identity-name">{idName}</div>
					<!-- QURATOR-379: the share-code and npub rows are gone — the card keeps the npub
					     (truncated, copies the FULL value) as its one line. The share code stays
					     reachable through Chat's "Share my code". -->
					<div class="identity-npub">
						<span class="identity-npub-text">{idNpubShort}</span>
						<button class="icon-btn" title="Copy npub" aria-label="Copy npub" onclick={() => handleCopy(idNpub)}>{@html icons.copy}</button>
					</div>
				</div>
			</div>

			{#if kv.showStorageWarning}
				<div class="key-storage-warn">
					{@html icons.key} Your key is stored as a protected file, not in an OS keyring. Anyone with access to your user account can read it.
				</div>
			{/if}
		</div>

		<!-- Backup / restore — QURATOR-379: ONE surface. Export half on top, a divider, then the
		     replace half. The Nostr-key import is deleted (owner, Option A): switching keys is
		     Danger zone → Wipe data → the wizard's "Use my Nostr key". -->
		<div class="section-label">Backup &amp; restore</div>
		<div class="surface">
			<div class="toggle-label">Export backup</div>
			<div class="toggle-sub">Keys, collections, contacts and settings. A lost identity can’t be recovered without one.</div>
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
				<div class="br-pass">
					<label class="field-label" for="backup-pass">Backup passphrase</label>
					<input id="backup-pass" class="hb-input" type="password" bind:value={backupPass} />
					{#if backupPass}
						<div class="strength-row">
							<div class="strength-bar"><div class="strength-fill" style="width:{backupStrength.score * 25}%" class:strength-bad={!backupStrength.acceptable}></div></div>
							<span class="strength-label">{backupStrength.label}</span>
						</div>
						{#if backupStrength.reason}<div class="toggle-sub">{backupStrength.reason}</div>{/if}
					{:else}
						<div class="toggle-sub">At least 12 characters.</div>
					{/if}
				</div>
			{/if}
			<div style="display:flex; gap:8px; flex-wrap:wrap;">
				<button class="btn-primary btn-sm" onclick={handleBackup} disabled={backingUp || (backupMode === 'passphrase' && !backupStrength.acceptable)}>
					{backingUp ? 'Exporting…' : 'Export backup'}
				</button>
			</div>

			<div class="br-divider"></div>

			<div class="toggle-label">Replace this identity</div>
			<div class="toggle-sub">Replaces all data on this device, then restarts.</div>
			<div style="display:flex; gap:8px; flex-wrap:wrap;">
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

		<!-- QURATOR-379 (owner, Option A): the "Use a different Nostr key" import section is gone.
		     Switching keys: Danger zone → Wipe data → the wizard's "Use my Nostr key". -->
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
				<div class="toggle-label">Allow incoming messages from anyone<FeatureTooltip key="allow-dms" /></div>
			</div>
			<button class="toggle" class:toggle-on={allowDms} onclick={toggleAllowDms} disabled={!settingsLoaded} aria-label="Allow incoming messages from anyone">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Auto-update snapshots on change<FeatureTooltip key="auto-update-snapshots" /></div>
			</div>
			<button class="toggle" class:toggle-on={snapshotAutoUpdate} onclick={() => toggleSetting('snapshot_auto_update')} disabled={!settingsLoaded} aria-label="Auto-update snapshots on change">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<!-- Owner ruling 2026-09-26: rewritten so the label alone says what this does — a periodic
		     re-check, on top of the file watcher — instead of naming its SMB audience first. The
		     "why you'd want this" detail moved into the tooltip. -->
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Periodically re-check published collections<FeatureTooltip key="reconcile-poll" /></div>
			</div>
			<button class="toggle" class:toggle-on={snapshotReconcilePoll} onclick={() => toggleSetting('snapshot_reconcile_poll')} disabled={!settingsLoaded} aria-label="Periodically re-check published collections">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Show up in Discover Hoarders<FeatureTooltip key="discoverable" /></div>
			</div>
			<button class="toggle" class:toggle-on={settings.discoverable} onclick={toggleDiscoverable} disabled={!settingsLoaded} aria-label="Show up in Discover Hoarders">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<!-- Owner devtest of v0.25.0 (2026-10-14): the reciprocity disclosure moves behind the (i) like
		     the other Preferences rows — this reverses QURATOR-164's "stays in plain view" ruling. -->
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Fetch new collections automatically<FeatureTooltip key="swarm-caching" /></div>
			</div>
			<button class="toggle" class:toggle-on={settings.swarm_caching} onclick={toggleSwarmCaching} disabled={!settingsLoaded} aria-label="Fetch new collections automatically">
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
		<div class="update-row" bind:this={updateRowEl}>
			<div class="toggle-text">
				<div class="toggle-label">App version</div>
				<!-- QURATOR-158: new-version indicator — visible ONLY when a check (background or
				     manual) actually found a newer version. Click-through is the existing update
				     flow to its right (the "vX.Y available" + Download/Update &amp; restart pair),
				     so this badge scrolls the same affordance into view rather than forking one.
				     jsdom computes no layout — placement above the sub-label is CSS, not tested. -->
				{#if isPortable ? portableInfo : updateInfo}
					<button class="update-badge" title="A newer version is available" onclick={() => updateRowEl?.scrollIntoView({ block: 'center' })}>
						<span class="update-badge-dot" aria-hidden="true"></span>
						New version available
					</button>
				{/if}
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
		<!-- QURATOR-176: deliberately NOT in the Danger zone — this is re-fetchable disk-space
		     cleanup, not data loss, and a danger framing would overclaim it. Also deliberately
		     one click with no confirm (owner ruling 2026-09-03). -->
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Clear manifest cache</div>
				<div class="toggle-sub">
					Removes every cached offline copy of browsed collections to free disk space. Nothing
					else is touched, and each copy re-downloads the next time you browse it.
				</div>
			</div>
			<button class="btn-default btn-sm" onclick={handleClearManifestCache} disabled={clearingCache}>
				{clearingCache ? 'Clearing…' : 'Clear cache'}
			</button>
		</div>
	</div>

	<!-- Watches section removed 2026-08-03: the backend (watches_get/create/delete/evaluate) exists
	     but no UI ever creates a watch — the save-search affordance in Discover was never built, so
	     the section was a permanently-empty list. Restore it when Discover grows "save this search". -->

	<!-- QURATOR-352: Appearance, after Diagnostics and before the Danger zone (owner, 2026-10-13). -->
	<div class="section-label">Appearance</div>
	<div class="surface">
		<!-- A radio group takes no tabindex itself: focus sits on the checked radio (roving tabindex
		     below), and this keydown only catches arrow keys bubbling up from it. -->
		<!-- svelte-ignore a11y_interactive_supports_focus -->
		<div class="theme-grid" role="radiogroup" aria-label="Theme" onkeydown={onThemeGridKeydown}>
			{#each THEMES as t (t.id)}
				<button type="button" class="theme-card" role="radio" aria-checked={t.id === themeId} tabindex={t.id === themeId ? 0 : -1} onclick={() => selectTheme(t.id)} disabled={!settingsLoaded}>
					<span class="theme-swatch" aria-hidden="true">
						<span class="theme-stripe" style="background: {t.swatch[0]}"></span>
						<span class="theme-stripe stripe-mid" style="background: {t.swatch[1]}"></span>
						<span class="theme-stripe" style="background: {t.swatch[2]}"></span>
					</span>
					<span class="theme-name">{t.name}</span>
					<span class="theme-meta"><span class="theme-source">{themeSubtitle(t)}</span>{#if t.mode === 'light'}<span class="theme-mode">Light</span>{/if}</span>
				</button>
			{/each}
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Glass effects</div>
			</div>
			<button class="toggle" class:toggle-on={glassOn} onclick={toggleGlass} disabled={!settingsLoaded} aria-label="Glass effects">
				<span class="toggle-thumb"></span>
			</button>
		</div>

	</div>

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
</div>

<!-- QURATOR-379: the one in-app confirm. Driven by `askInApp` above; the plugin's native
     `confirm` is deleted from this page (owner ruling: native popups read as an app fault).
     Esc/backdrop run Modal's onclose → settleConfirm(false). -->
{#if confirmState}
	{@const cs = confirmState}
	<Modal open={true} title={cs.title} onclose={() => settleConfirm(false)} width="420px">
		<p class="confirm-body">{cs.body}</p>
		{#snippet actions()}
			<button class="btn-ghost btn-sm" onclick={() => settleConfirm(false)}>Cancel</button>
			<button class={cs.danger ? 'btn-danger' : 'btn-primary'} onclick={() => settleConfirm(true)}>{cs.okLabel}</button>
		{/snippet}
	</Modal>
{/if}

<style>
	.confirm-body { font-size: 13px; color: var(--fg-muted); line-height: 1.5; margin: 0; }
	.topbar {
		padding: 16px 24px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		flex-shrink: 0;
	}
	/* QURATOR-352 — the divider between panes disappears when panes float (hb-pane supplies the
	   glass rim instead). In hoard (no data-glass-float) it stays. */
	:global(:root[data-glass-float]) .topbar { border-bottom: 0; }
	.topbar-title { font-size: 17px; font-weight: 600; letter-spacing: -0.3px; }
	.topbar-sub { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }

	/* The pane itself never scrolls: the glass paints on .hb-pane::before (app.css), an absolute
	   layer inset to the pane's visible box — inside a scroll container that is one screenful, so
	   the glass cut off at the first scroll boundary (owner, 2026-10-13). .body is a fixed flex
	   column; .body-scroll carries the overflow and the padding. */
	.body { flex: 1; overflow: hidden; display: flex; flex-direction: column; }
	.body-scroll { flex: 1; min-width: 0; padding: 24px; padding-right: max(24px, calc(100% - 696px)); overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
	/* QURATOR-352 + QURATOR-369 — the pane fills the width in EVERY mode (glass on or off, float
	   theme or hoard); the right padding keeps the CONTENT column at 720px (24px left padding +
	   672px content + the rest as right padding). Before 369 the 720px cap lived in the base rule
	   and the uncap was a float-only override, so glass off retracted the pane to the cap while
	   glass on extended it to the edge — the owner's report. Making the full-width shape the base
	   rule and deleting the override gives hoard-without-glass the same extent, per the order:
	   the owner wants the same extent in both modes. */

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
		border-radius: var(--r-card, 10px);
		padding: 18px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.surface-nop { padding: 0; gap: 0; }

	.danger-surface { border-color: color-mix(in oklch, var(--error) 30%, transparent); }

	.identity-top { display: flex; gap: 16px; align-items: center; }

	.identity-info { flex: 1; min-width: 0; }

	.identity-name { font-size: 14px; font-weight: 600; }

	/* QURATOR-379 — the card's one key line: truncated npub, mono, muted, copies the full value. */
	.identity-npub { display: flex; align-items: center; gap: 6px; margin-top: 2px; min-width: 0; }
	.identity-npub-text {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--fg-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		min-width: 0;
	}

	.key-storage-warn {
		margin-top: 12px;
		padding: 10px 12px;
		border: 1px solid color-mix(in oklch, var(--accent) 35%, transparent);
		background: color-mix(in oklch, var(--accent) 8%, transparent);
		border-radius: var(--r-control, 7px);
		font-size: 12px;
		color: var(--fg-muted);
		display: flex;
		gap: 8px;
		align-items: flex-start;
		line-height: 1.5;
	}

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
		/* Amber, not the neutral accent tone — a warning that this network shape may need a relay
		   or block a direct transfer (matches the amber convention used elsewhere: the online-chip
		   dot and the cold-cache control, both at this hue). */
		color: oklch(0.78 0.15 75);
		border-color: oklch(0.5 0.15 75 / 0.4);
		background: oklch(0.28 0.08 75 / 0.5);
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
	/* QURATOR-158 — new-version indicator, above "Currently running v…". Unobtrusive by design:
	   a small accent dot + label. Routing = scrollIntoView on the row that owns the real update
	   buttons (right there on the same row), not a second updater UI. */
	.update-badge {
		display: inline-flex; align-items: center; gap: 6px;
		border: none; background: none; padding: 0; margin: 2px 0 3px;
		font: inherit; font-size: 12px; font-weight: 600; color: var(--accent);
		cursor: pointer;
	}
	.update-badge:hover { text-decoration: underline; }
	.update-badge-dot {
		width: 8px; height: 8px; border-radius: 50%;
		background: var(--accent);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 25%, transparent);
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

	/* QURATOR-379 — Backup & restore: one surface, two halves. */
	.br-divider { height: 1px; background: var(--border); flex-shrink: 0; }
	.br-pass { display: flex; flex-direction: column; align-items: stretch; gap: 4px; }
	.backup-modes { display: flex; flex-direction: column; gap: 8px; }
	.backup-mode {
		display: flex; gap: 10px; align-items: flex-start; padding: 10px 12px;
		border: 1px solid var(--border); border-radius: var(--r-card, 8px); cursor: pointer;
	}
	.backup-mode-on { border-color: var(--accent); background: color-mix(in oklch, var(--accent) 7%, transparent); }
	.backup-mode-label { font-size: 12.5px; font-weight: 500; color: var(--fg); }
	.strength-row { display: flex; align-items: center; gap: 10px; }
	.strength-bar { flex: 1; height: 6px; border-radius: 99px; background: var(--bg-elev3); overflow: hidden; }
	.strength-fill { height: 100%; background: var(--online); transition: width 0.15s; }
	.strength-fill.strength-bad { background: var(--error); }
	.strength-label { font-size: 11px; color: var(--fg-dim); white-space: nowrap; }
	.restore-pass { display: flex; gap: 8px; align-items: center; margin-top: 4px; }

	/* ── QURATOR-352 Appearance ────────────────────────────────────────────── */
	.theme-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
		gap: 8px;
		margin-bottom: 12px;
	}
	.theme-card {
		display: flex; flex-direction: column; align-items: stretch; gap: 6px;
		padding: 10px; text-align: left; cursor: pointer; font: inherit;
		border: 1px solid var(--border); border-radius: var(--r-card, 8px);
		background: var(--bg-elev1); color: var(--fg);
	}
	.theme-card[aria-checked="true"] {
		border-color: var(--accent);
		background: color-mix(in oklch, var(--accent) 7%, transparent);
	}
	.theme-card:disabled { opacity: 0.6; cursor: default; }
	.theme-swatch {
		display: flex; width: 100%; height: 28px;
		border-radius: 5px; overflow: hidden;
		border: 1px solid var(--border);
	}
	.theme-stripe { flex: 1; }
	.theme-name { font-size: 12.5px; font-weight: 500; color: var(--fg); }
	.theme-meta { display: flex; gap: 6px; align-items: center; font-size: 11px; color: var(--fg-muted); }
	.theme-mode {
		border: 1px solid var(--border); border-radius: 99px; padding: 0 6px;
		color: var(--fg-dim);
	}
</style>
