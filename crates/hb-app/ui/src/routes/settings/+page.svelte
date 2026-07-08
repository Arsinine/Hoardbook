<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { generateKeypair, getShareCode, getSettings, saveSettings, importNsec, backupData, peekBackup, restoreData, wipeData, checkRelay, relayStatus, checkUpdate, downloadUpdate, applyStagedUpdate, takeUpdateNotice, watchesGet, watchesDelete } from '$lib/api.js';
	import type { Settings, UpdateInfo } from '$lib/api.js';
	import type { Watch } from '$lib/types.js';
	import { keyView } from '$lib/key-view.js';
	import { passphraseStrength, backupModeOptions, type BackupMode } from '$lib/backup-export.js';
	import { updateNoticeVM } from '$lib/update-ux.js';
	import { DEFAULT_RELAYS, validateRelayUrl } from '$lib/relays.js';
	import QRCode from 'qrcode';
	import { relaunch } from '@tauri-apps/plugin-process';
	import { open as openFileDialog, save as saveFileDialog, confirm } from '@tauri-apps/plugin-dialog';
	import { getVersion } from '@tauri-apps/api/app';
	import { identity, profile, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';

	let generating = $state(false);
	let copied = $state(false);
	let appVersion = $state('');

	// Full settings object, preserved so saving one field never resets the others (the M5 fields
	// privacy_notice_acknowledged / last_seen_version + the M9 snapshot/online toggles live here too).
	let settings: Settings = $state({
		relay_urls: [], allow_dms: true, privacy_notice_acknowledged: false,
		last_seen_version: '',
		snapshot_auto_update: true, snapshot_reconcile_poll: false, show_online_count: true,
	});

	// ── 3-key identity view + share-code QR ──────────────────────────────────────
	let kv = $derived($identity ? keyView($identity) : null);
	let shareQrSvg = $state('');

	async function showShareQr() {
		try {
			const code = await getShareCode();
			shareQrSvg = shareQrSvg ? '' : await QRCode.toString(code, { type: 'svg', margin: 1 });
		} catch (e) { toast(String(e), 'error'); }
	}

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
				'This backup is UNENCRYPTED — the file IS your identity. Anyone who obtains it becomes you. Store it like a master key. Continue?',
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
		if (!restorePath) return;
		const ok = await confirm(
			'Restoring REPLACES all current data on this device with the backup, then restarts. Continue?',
			{ title: 'Restore from backup', kind: 'warning' },
		);
		if (!ok) return;
		restoring = true;
		try {
			await wipeData();
			const info = await restoreData(restoreNeedsPass ? restorePass : null, restorePath);
			identity.set(info);
			restorePath = null; restorePass = ''; restoreNeedsPass = false;
			toast('Backup restored — restarting…');
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

	async function doCheckUpdate() {
		updateChecking = true;
		updateError = '';
		updateInfo = null;
		updateChecked = false;
		try {
			updateInfo = await checkUpdate();
			updateChecked = true;
		} catch (e) {
			updateError = String(e).replace(/^Error: /, '');
		} finally {
			updateChecking = false;
		}
	}

	// Background download + minisign-verify, staged for deferred install — NO immediate restart.
	async function doDownloadUpdate() {
		updateStaging = true;
		try {
			stagedVersion = await downloadUpdate();
			if (stagedVersion) {
				toast(`Update v${stagedVersion} downloaded — it applies when you restart`, 'success');
			}
		} catch (e) { toast(String(e), 'error'); }
		finally { updateStaging = false; }
	}

	async function doApplyUpdate() {
		try { await applyStagedUpdate(); } catch (e) { toast(String(e), 'error'); }
	}

	let relayUrls: string[] = $state([]);
	let newRelay = $state('');
	let savingRelays = $state(false);
	let addingRelay = $state(false);

	type RelayStatus = 'checking' | 'ok' | 'error';
	let relayStatuses: Record<string, RelayStatus> = $state({});

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
	}
	onDestroy(() => { if (liveStatusTimer) clearInterval(liveStatusTimer); });

	let allowDms = $derived(settings.allow_dms);

	let wipeConfirm = $state(false);
	let wiping = $state(false);

	onMount(async () => {
		try { appVersion = await getVersion(); } catch { appVersion = ''; }
		loadWatches();
		try {
			settings = await getSettings();
			// Fresh install has no saved relays — show the curated public defaults (the backend
			// falls back to the same set). The user can edit or remove them.
			relayUrls = settings.relay_urls.length ? settings.relay_urls : [...DEFAULT_RELAYS];
			relayUrls.forEach(probeRelay);
			// Overlay the live data-path status once the persistent client has had a moment to dial,
			// then keep it current on a slow tick while the page is open.
			refreshLiveRelayStatus();
			liveStatusTimer = setInterval(refreshLiveRelayStatus, 12_000);
		} catch { /* proceed with defaults if settings load fails */ }
		// Visible-after "now running vX.Y" notice — fires once per version change.
		try {
			const notice = updateNoticeVM(await takeUpdateNotice());
			if (notice.show) toast(`Now running v${notice.version} — see the changelog for what's new`, 'success');
		} catch { /* updater not configured */ }
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

	async function toggleAllowDms() {
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
		settings = { ...settings, [field]: !settings[field] };
		try {
			await saveSettings(fullSettings());
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

	// Watches
	let watches: Watch[] = $state([]);

	async function loadWatches() {
		try { watches = await watchesGet(); } catch { /* no watches */ }
	}

	async function handleDeleteWatch(name: string) {
		try {
			await watchesDelete(name);
			watches = watches.filter(w => w.name !== name);
			toast(`Watch "${name}" deleted`);
		} catch (e) { toast(String(e), 'error'); }
	}

	function formatWatchDate(iso: string | undefined): string {
		if (!iso) return 'Never';
		return new Date(iso).toLocaleDateString();
	}

	let idName = $derived($profile?.display_name ?? 'You');
	let idInitial = $derived(idName[0]?.toUpperCase() ?? 'Y');
	let idHue = $derived(avatarHue(idInitial));

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
<div class="topbar">
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
				<Avatar letter={idInitial} size={56} hue={idHue} />
				<div class="identity-info">
					<div class="identity-name">{idName}</div>
					<div class="identity-created">Nostr identity (npub)</div>
				</div>
				<span class="pill pill-online"><span class="pill-dot"></span>Active</span>
			</div>

			<!-- The three keys: npub (irreplaceable), iroh node key (public), share code (carries the browse-key). -->
			{#each kv.rows as row (row.label)}
				<div class="field-label" style="margin-bottom:4px">{row.label}{#if row.sensitive} <span class="key-secret">secret</span>{/if}</div>
				<div class="id-display">
					<span class="id-text">{row.value}</span>
					<button class="icon-btn" onclick={() => handleCopy(row.value)} title={row.label === 'Share code' ? 'Copy share code' : 'Copy npub'}>{@html icons.copy}</button>
					{#if row.label === 'Share code'}
						<button class="icon-btn" onclick={showShareQr} title="Show QR code">{@html icons.qr}</button>
					{/if}
				</div>
				{#if row.hint}<div class="id-hint" style="margin-bottom:12px">{row.hint}</div>{/if}
			{/each}

			{#if shareQrSvg}
				<div class="qr-box">{@html shareQrSvg}</div>
				<div class="id-hint">Scan to import your share code on another device. Treat it as secret — it unlocks your listings.</div>
			{/if}
			{#if copied}
				<div class="id-actions"><span class="id-hint">Copied!</span></div>
			{/if}

			<div class="no-recovery">{@html icons.key} {kv.noRecoveryNotice}</div>

			{#if kv.showStorageWarning}
				<div class="key-storage-warn">
					{@html icons.key} Your key is stored as a protected file ({kv.storageLabel}), not in an
					OS keyring on this platform. Anyone with access to your user account can read it —
					keep this device and your home directory secure. Keyring support is planned.
				</div>
			{/if}
		</div>

		<!-- Backup / restore -->
		<div class="section-label">Backup &amp; restore</div>
		<div class="surface">
			<div class="field-label">Export a portable backup of your whole profile (all three keys + collections, contacts, settings). Store it somewhere safe — it is your only protection against losing your identity.</div>
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
					<input class="hb-input" type="password" placeholder="Backup passphrase" bind:value={restorePass} />
					<button class="btn-primary btn-sm" onclick={doRestore} disabled={!restorePass || restoring}>Restore</button>
					<button class="btn-ghost btn-sm" onclick={() => { restorePath = null; restoreNeedsPass = false; }}>Cancel</button>
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
					{@html icons.key} <strong>Linking warning:</strong> if this key is public — or the
					same key you use in Qurator or anywhere else — importing it links that identity to your
					Hoardbook activity and de-pseudonymizes you. Only continue if you understand this.
				</div>
				<label class="ack-row"><input type="checkbox" bind:checked={importWarnAck} /> I understand the linking implication.</label>
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
					<input class="hb-input" type="password" placeholder="Backup passphrase" bind:value={restorePass} />
					<button class="btn-primary btn-sm" onclick={doRestore} disabled={!restorePass || restoring}>Restore</button>
					<button class="btn-ghost btn-sm" onclick={() => { restorePath = null; restoreNeedsPass = false; }}>Cancel</button>
				</div>
			{/if}
		</div>
	{/if}

	<!-- Relays -->
	<div class="section-row">
		<div class="section-label">Relays</div>
	</div>

	<div class="surface surface-nop">
		{#each relayUrls as url (url)}
			{@const status = relayStatuses[url]}
			<div class="relay-row">
				<div class="relay-dot" style="background:{relayDotColor(status)}" class:relay-dot-pulse={status === 'checking'}></div>
				<div class="relay-info">
					<div class="relay-url">{url}</div>
					<div class="relay-meta">
						<span class:status-ok={status === 'ok'} class:status-err={status === 'error'}>{relayStatusLabel(status)}</span>
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
			<button class="btn-primary btn-sm" onclick={handleSaveRelays} disabled={savingRelays}>
				{savingRelays ? 'Saving…' : 'Save'}
			</button>
		</div>
	</div>

	<!-- Preferences -->
	<div class="section-label">Preferences</div>

	<div class="surface">
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Allow incoming messages from anyone</div>
				<div class="toggle-sub">Off means only your contacts can DM you</div>
			</div>
			<button class="toggle" class:toggle-on={allowDms} onclick={toggleAllowDms} aria-label="Allow incoming messages from anyone">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Auto-update snapshots on change</div>
				<div class="toggle-sub">
					Re-publish a published listing when its folder changes (filesystem-watch). Off = manual
					"Regenerate" only. Note: a watch sees your local edits, not server-side changes another
					host makes on an SMB share — those reconcile on launch.
				</div>
			</div>
			<button class="toggle" class:toggle-on={snapshotAutoUpdate} onclick={() => toggleSetting('snapshot_auto_update')} aria-label="Auto-update snapshots on change">
				<span class="toggle-thumb"></span>
			</button>
		</div>

		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Reconcile poll for remotely-edited collections</div>
				<div class="toggle-sub">Low-frequency re-check for collections you edit from another host (SMB). Off by default.</div>
			</div>
			<button class="toggle" class:toggle-on={snapshotReconcilePoll} onclick={() => toggleSetting('snapshot_reconcile_poll')} aria-label="Reconcile poll for remotely-edited collections">
				<span class="toggle-thumb"></span>
			</button>
		</div>
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
				{#if stagedVersion}
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
		{#if stagedVersion}
			<div class="toggle-sub">Downloaded and verified. It installs automatically when you quit Hoardbook (or click "Restart &amp; apply").</div>
		{/if}
		{#if updateError}
			<div class="update-error-text">{updateError}</div>
		{/if}
	</div>

	<!-- Watches -->
	<div class="section-label">Watches</div>

	<div class="surface">
		{#if watches.length === 0}
			<div class="watches-empty">No saved watches. A watch is a saved tag search that flags new matching hoarders as they appear.</div>
		{:else}
			<div class="watch-list">
				{#each watches as w (w.name)}
					<div class="watch-row-item">
						<div class="watch-info">
							<div class="watch-name">{w.name}</div>
							<div class="watch-detail">
								{#if w.content_types.length > 0}
									<span class="watch-chip">{w.content_types.join(', ')}</span>
								{/if}
								{#if w.tags.length > 0}
									<span class="watch-chip tags">#{w.tags.join(', #')}</span>
								{/if}
								<span class="watch-fired">Last triggered: {formatWatchDate(w.last_fired)}</span>
							</div>
						</div>
						<button class="btn-ghost btn-sm btn-danger-text" onclick={() => handleDeleteWatch(w.name)}>Delete</button>
					</div>
				{/each}
			</div>
		{/if}
	</div>

	<!-- Danger Zone -->
	<div class="section-label danger-label">Danger zone</div>

	<div class="surface danger-surface">
		<div class="danger-row">
			<div>
				<div class="toggle-label">Wipe all data</div>
				<div class="toggle-sub">Permanently removes your identity, profile, and app data from this device. Your actual files on disk are not touched — only Hoardbook's database is cleared.</div>
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

	.identity-created { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }

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
		flex: 1;
	}
	.hb-input::placeholder { color: var(--fg-dim); }
	.hb-input:focus { border-color: var(--accent); }
	.hb-mono { font-family: var(--font-mono); }


	/* Toggles */
	.toggle-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; }

	.toggle-text { flex: 1; }

	.toggle-label { font-size: 12.5px; color: var(--fg); font-weight: 500; }

	.toggle-sub { font-size: 11px; color: var(--fg-dim); margin-top: 1px; }

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

	.toggle-thumb {
		position: absolute;
		top: 1px; left: 1px;
		width: 13px; height: 13px;
		border-radius: 50%;
		background: var(--fg-muted);
		transition: left 0.15s, background 0.15s;
	}
	.toggle-on .toggle-thumb { left: 14px; background: var(--accent-text); }

	/* Watches */
	.watches-empty { font-size: 12.5px; color: var(--fg-dim); padding: 4px 0; }

	.watch-list { display: flex; flex-direction: column; gap: 1px; }

	.watch-row-item {
		display: flex; align-items: center; gap: 10px;
		padding: 10px 0;
		border-bottom: 1px solid var(--divider);
	}
	.watch-row-item:last-child { border-bottom: none; }

	.watch-info { flex: 1; min-width: 0; }

	.watch-name { font-size: 13px; font-weight: 500; color: var(--fg); }

	.watch-detail { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 3px; align-items: center; }

	.watch-chip {
		font-size: 10.5px; padding: 1px 7px; border-radius: 4px;
		background: color-mix(in oklch, var(--accent) 12%, transparent);
		color: var(--accent);
		border: 1px solid color-mix(in oklch, var(--accent) 20%, transparent);
	}
	.watch-chip.tags { background: var(--bg-elev3); color: var(--fg-muted); border-color: var(--border); }

	.watch-fired { font-size: 10.5px; color: var(--fg-dim); }

	.btn-danger-text { color: var(--red, #e05c5c); }

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
	.btn-primary {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}
	.btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
	.btn-default {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg); background: var(--bg-elev2);
		border: 1px solid var(--border-strong); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
		flex-shrink: 0; min-width: max-content;
	}
	.btn-default:hover { background: var(--bg-elev3); }
	.btn-default:disabled { opacity: 0.5; cursor: not-allowed; }
	.btn-ghost {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg-muted); background: transparent;
		border: 1px solid transparent; border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}
	.btn-danger {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: oklch(0.97 0 0); background: var(--error);
		border: 1px solid var(--error); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}
	.btn-danger:disabled { opacity: 0.5; cursor: not-allowed; }
	.btn-sm { padding: 5px 11px; font-size: 12px; height: 28px; }

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
	.qr-box {
		display: flex; justify-content: center; padding: 12px;
		background: oklch(0.98 0 0); border-radius: 8px; margin-top: 8px;
	}
	.qr-box :global(svg) { width: 180px; height: 180px; }
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
