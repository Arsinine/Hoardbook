<script lang="ts">
	import { onMount } from 'svelte';
	import { generateKeypair, getHbId, getSettings, saveSettings, saveKeypairFile, importKeypair, wipeData, checkRelay } from '$lib/api.js';
	import { relaunch } from '@tauri-apps/plugin-process';
	import { save, open as openFileDialog } from '@tauri-apps/plugin-dialog';
	import { identity, profile, collections, contacts, toast } from '$lib/stores.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';

	let generating = false;
	let copied = false;
	let exporting = false;
	let importing = false;

	let relayUrls: string[] = [];
	let newRelay = '';
	let savingRelays = false;
	let addingRelay = false;

	type RelayStatus = 'checking' | 'ok' | 'error';
	let relayStatuses: Record<string, RelayStatus> = {};

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

	let allowDms = true;
	let recommended = false;
	let settingsLoaded = false;

	let wipeConfirm = false;
	let wiping = false;

	onMount(async () => {
		try {
			const s = await getSettings();
			relayUrls = s.relay_urls;
			allowDms = s.allow_dms ?? true;
			recommended = s.recommended ?? false;
			settingsLoaded = true;
			// Probe all saved relays in parallel.
			relayUrls.forEach(probeRelay);
		} catch { settingsLoaded = true; }
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

	async function handleCopy() {
		try {
			const id = await getHbId();
			// Try the modern clipboard API first; fall back to execCommand for
			// environments where navigator.clipboard is restricted.
			try {
				await navigator.clipboard.writeText(id);
			} catch {
				const el = document.createElement('textarea');
				el.value = id;
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

	async function handleExport() {
		exporting = true;
		try {
			const path = await save({
				defaultPath: 'hoardbook-keypair.json',
				filters: [{ name: 'JSON', extensions: ['json'] }],
			});
			if (!path) return;
			await saveKeypairFile(path);
			toast(`Keypair saved to ${path} — contains your private key, guard this file`);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			exporting = false;
		}
	}

	async function handleImport() {
		importing = true;
		try {
			const path = await openFileDialog({
				multiple: false,
				filters: [{ name: 'Hoardbook keypair', extensions: ['json'] }],
			});
			if (!path) return;
			const info = await importKeypair(path as string);
			identity.set(info);
			toast('Keypair imported — your identity has been restored');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			importing = false;
		}
	}

	async function toggleAllowDms() {
		allowDms = !allowDms;
		try {
			await saveSettings({ relay_urls: relayUrls, allow_dms: allowDms, recommended });
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function toggleRecommended() {
		recommended = !recommended;
		try {
			await saveSettings({ relay_urls: relayUrls, allow_dms: allowDms, recommended });
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleWipe() {
		wiping = true;
		try {
			await wipeData();
			await relaunch();
		} catch (e) {
			toast(String(e), 'error');
			wiping = false;
		}
	}

	async function addRelay() {
		const url = newRelay.trim().replace(/\/$/, '');
		if (!url || relayUrls.includes(url)) return;
		if (!url.startsWith('http://') && !url.startsWith('https://')) {
			toast('Relay URL must start with http:// or https://', 'error');
			return;
		}
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
			await saveSettings({ relay_urls: relayUrls, allow_dms: allowDms, recommended });
			toast('Relay settings saved');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			savingRelays = false;
		}
	}

	$: idName = $profile?.display_name ?? 'You';
	$: idInitial = idName[0]?.toUpperCase() ?? 'Y';
	$: idHue = avatarHue(idInitial);

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

	{#if $identity}
		<div class="surface">
			<div class="identity-top">
				<Avatar letter={idInitial} size={56} hue={idHue} />
				<div class="identity-info">
					<div class="identity-name">{idName}</div>
					<div class="identity-created">Ed25519 keypair</div>
				</div>
				<span class="pill pill-online"><span class="pill-dot" />Active</span>
			</div>

			<div class="field-label" style="margin-bottom:6px">Your Hoardbook ID</div>
			<div class="id-display">
				<span class="id-text">{$identity.hb_id}</span>
				<button class="icon-btn" on:click={handleCopy} title="Copy">{@html icons.copy}</button>
			</div>

			<div class="id-actions">
				<span class="id-hint">Share this so others can look you up.</span>
				<div class="id-btns">
					<button class="btn-default btn-sm btn-icon" on:click={handleCopy}>
						{@html icons.copy} {copied ? 'Copied!' : 'Copy'}
					</button>
					<button class="btn-default btn-sm btn-icon" on:click={handleExport} disabled={exporting}>
						{@html icons.key} {exporting ? 'Exporting…' : 'Export key'}
					</button>
				</div>
			</div>
		</div>
	{:else}
		<div class="surface">
			<p class="no-id-text">No identity yet. Generate a keypair or restore from a backup.</p>
			<div style="display:flex; gap:8px; flex-wrap:wrap;">
				<button class="btn-primary" on:click={handleGenerate} disabled={generating}>
					{generating ? 'Generating…' : 'Generate keypair'}
				</button>
				<button class="btn-default" on:click={handleImport} disabled={importing}>
					{@html icons.key} {importing ? 'Importing…' : 'Import from backup'}
				</button>
			</div>
		</div>
	{/if}

	<!-- Relays -->
	<div class="section-row">
		<div class="section-label">Relays</div>
	</div>

	<div class="surface surface-nop">
		{#if relayUrls.length === 0}
			<div class="relay-empty">No relays configured. Add one to publish and browse.</div>
		{:else}
			{#each relayUrls as url}
				{@const status = relayStatuses[url]}
				<div class="relay-row">
					<div class="relay-dot" style="background:{relayDotColor(status)}" class:relay-dot-pulse={status === 'checking'} />
					<div class="relay-info">
						<div class="relay-url">{url}</div>
						<div class="relay-meta">
							<span class:status-ok={status === 'ok'} class:status-err={status === 'error'}>{relayStatusLabel(status)}</span>
						</div>
					</div>
					<button class="icon-btn" title="Re-check" on:click={() => probeRelay(url)}>{@html icons.refresh}</button>
					<button class="icon-btn" on:click={() => removeRelay(url)}>{@html icons.close}</button>
				</div>
			{/each}
		{/if}
		<!-- Add relay row -->
		<div class="relay-add-row">
			<input
				class="hb-input hb-mono"
				type="text"
				placeholder="http://relay.example.com:3000"
				bind:value={newRelay}
				on:keydown={(e) => e.key === 'Enter' && addRelay()}
			/>
			<button class="btn-default btn-sm" on:click={addRelay} disabled={!newRelay.trim() || addingRelay}>
				{addingRelay ? 'Checking…' : 'Add'}
			</button>
			<button class="btn-primary btn-sm" on:click={handleSaveRelays} disabled={savingRelays}>
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
				<div class="toggle-sub">Off means only people you follow can DM you</div>
			</div>
			<button class="toggle" class:toggle-on={allowDms} on:click={toggleAllowDms}>
				<span class="toggle-thumb" />
			</button>
		</div>
		<div class="surface-divider" />
		<div class="toggle-row">
			<div class="toggle-text">
				<div class="toggle-label">Appear in Recommended contacts</div>
				<div class="toggle-sub">Others can discover you without needing your hb_id. Requires relay support.</div>
			</div>
			<button class="toggle" class:toggle-on={recommended} on:click={toggleRecommended}>
				<span class="toggle-thumb" />
			</button>
		</div>
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
				<button class="btn-danger btn-sm" on:click={() => (wipeConfirm = true)}>Wipe data</button>
			{:else}
				<div class="wipe-confirm">
					<span class="wipe-warn">Are you sure? This is permanent.</span>
					<button class="btn-danger btn-sm" on:click={handleWipe} disabled={wiping}>
						{wiping ? 'Wiping…' : 'Confirm wipe'}
					</button>
					<button class="btn-ghost btn-sm" on:click={() => (wipeConfirm = false)}>Cancel</button>
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

	.id-btns { display: flex; gap: 8px; flex-shrink: 0; align-items: center; }

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

	.relay-empty { padding: 14px 16px; font-size: 12.5px; color: var(--fg-dim); }

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

	.surface-divider { height: 1px; background: var(--divider); margin: 4px 0; }

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
	.btn-icon { gap: 5px; }
</style>
