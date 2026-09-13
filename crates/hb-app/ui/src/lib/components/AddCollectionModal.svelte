<script lang="ts">
	// QURATOR-206 — ONE two-column screen (owner ruling + approved mockup, 2026-09-13). The old
	// two-step wizard (ScanDialog → step swap → CollectionDetailsForm modal) is merged: left column
	// "Directory" owns the path/Browse, display name, include/exclude tree, Start scan and the scan
	// summary; right column "Details" owns content types, tags, languages, notes, Sorted/Private.
	// One header, one Cancel/Publish footer spanning both. "Start scan" no longer swaps modals — it
	// fills the scan summary while the Details column stays exactly where it was. A row menu's
	// Rescan / Edit details opens THIS screen pre-loaded (`editCollection` + `initialPath`; the tree
	// is pre-resolved), so add, rescan and edit share one screen.
	//
	// QURATOR-207 model: a scan persists NOTHING durable — `collection` below is the backend's
	// in-memory cache handle. Cancel/×/Esc discards by construction; Publish (updateCollectionMeta →
	// publishCollection, which promotes the cached scan) is the single durable write. There is no
	// draft save and must not be one.
	import { open as openDialog } from '@tauri-apps/plugin-dialog';
	import {
		scanDirectory,
		listSubdirs,
		updateCollectionMeta,
		updateCollectionVisibility,
		publishCollection
	} from '../api.js';
	import { toast } from '../stores.js';
	import { icons } from '$lib/icons.js';
	import { serializeInclude, selectAllTopLevel } from '../scan-tree.js';
	import { toggleContentType } from '../content-types.js';
	import { MAX_DESCRIPTION_CHARS, MAX_LANGUAGES, MAX_LIST_ITEM_CHARS } from '../limits.js';
	import ScanTreeNode from './ScanTreeNode.svelte';
	import CollectionTagsEditor from './CollectionTagsEditor.svelte';
	import HintMarker from './HintMarker.svelte';
	import Modal from './Modal.svelte';
	import type { Collection, SubdirEntry, Visibility } from '../types.js';

	interface Props {
		open?: boolean;
		/** When set, the screen opens pre-loaded with this collection (Rescan / Edit details). */
		editCollection?: Collection | null;
		/** Root path used to pre-resolve the tree when reopening an existing collection. */
		initialPath?: string;
		onscanned?: (collection: Collection) => void;
		onpublished?: (collection: Collection) => void;
		onclose?: () => void;
	}

	let {
		open = $bindable(false),
		editCollection = null,
		initialPath = '',
		onscanned,
		onpublished,
		onclose
	}: Props = $props();

	// ── Left column (Directory) — carried over from ScanDialog ───────────────────
	let path = $state('');
	let pathAlias = $state('');
	let excludeRaw = $state('');
	let scanning = $state(false);

	// Folder-tree picker state (M8 — replaces the scan-depth slider, HANDOVER §A2.1).
	let topLevel: SubdirEntry[] | null = $state(null);
	let checked = $state(new Set<string>());
	let treeLoading = $state(false);
	let treeError = $state('');
	let loadedPath = '';

	// The last completed scan. QURATOR-207: nothing durable — just the cache handle Publish
	// promotes. Null until the first scan of this run (or seeded from `editCollection` on reopen).
	let collection: Collection | null = $state(null);
	// Captured at scan time so the summary reports the run that produced it, not today's checkboxes.
	let scanFolders = $state(0);

	// ── Right column (Details) — carried over from CollectionDetailsForm ────────
	// Same fixed six-value enum as the profile/discover pickers (HOARDBOOK_SPEC §4).
	const CONTENT_TYPES: { value: string; label: string }[] = [
		{ value: 'video', label: 'Video' },
		{ value: 'audio', label: 'Audio' },
		{ value: 'image', label: 'Image' },
		{ value: 'text', label: 'Text' },
		{ value: 'software', label: 'Software' },
		{ value: 'other', label: 'Other' }
	];

	let contentTypes: string[] = $state([]);
	let tags: string[] = $state([]);
	let languages: string[] = $state([]);
	let langInput = $state('');
	let notes = $state('');
	let sorted = $state(false);
	let isPrivate = $state(false);
	let publishing = $state(false);

	// Not reactive on purpose — a plain transition-edge flag, never read by the template, so it
	// mustn't be part of the effect's own dependency tracking (avoids a self-triggering effect).
	let wasOpen = false;
	$effect(() => {
		if (open && !wasOpen) {
			wasOpen = true;
			if (editCollection) {
				// Reopen pre-loaded (Rescan / Edit details): seed BOTH columns from the existing
				// collection and pre-resolve the tree when the caller knows the root path. Details
				// are seeded here and only here — a completed scan never reseeds them, so anything
				// the user typed while a scan ran survives it.
				collection = editCollection;
				path = initialPath;
				pathAlias = editCollection.path_alias;
				contentTypes = [...(editCollection.content_types ?? [])];
				tags = [...(editCollection.tags ?? [])];
				languages = [...(editCollection.languages ?? [])];
				notes = editCollection.description ?? '';
				sorted = editCollection.sorted ?? false;
				isPrivate = (editCollection.visibility ?? 'Public') === 'Private';
				scanFolders = 0;
				if (initialPath) loadTopLevel(initialPath);
			}
			// else: a fresh Add run — close() already reset everything on the way out.
		} else if (!open && wasOpen) {
			wasOpen = false;
		}
	});

	// Publish needs something to publish (a scan for Add, the existing collection for a reopen)
	// AND the standing ≥1-content-type gate from the old details form.
	let canPublish = $derived(collection !== null && contentTypes.length > 0);

	let selectedCount = $derived(checked.size);

	async function loadTopLevel(p: string) {
		if (!p) return;
		treeLoading = true;
		treeError = '';
		checked = new Set();
		topLevel = null;
		try {
			topLevel = await listSubdirs(p);
			loadedPath = p;
		} catch (e) {
			treeError = String(e);
			topLevel = [];
		} finally {
			treeLoading = false;
		}
	}

	async function browse() {
		const selected = await openDialog({ directory: true, multiple: false, title: 'Select directory' });
		if (selected) {
			path = selected as string;
			await loadTopLevel(path);
		}
	}

	function toggleCheck(rel: string) {
		if (checked.has(rel)) checked.delete(rel);
		else checked.add(rel);
		checked = checked; // reassign so reactivity flows through the recursive tree
	}

	function selectAll() {
		// devtest #10: "Select all" checks the top-level folders only — root-level files are always
		// included anyway, so adding them to the set would be redundant noise.
		if (topLevel) checked = selectAllTopLevel(topLevel.filter((n) => !n.is_file).map((n) => n.name));
	}

	function clearAll() {
		checked = new Set();
	}

	async function handleScan() {
		if (!path || !pathAlias) return;
		scanning = true;
		try {
			const exclude = excludeRaw.split(',').map((s) => s.trim()).filter(Boolean);
			const include = serializeInclude(checked);
			const scanned = await scanDirectory({ path, path_alias: pathAlias, include, exclude });
			// Only report the result if the screen is still open (user didn't cancel mid-scan).
			if (open) {
				collection = scanned;
				scanFolders = checked.size;
				onscanned?.(scanned);
			}
		} catch (e) {
			if (open) toast(String(e), 'error');
		} finally {
			scanning = false;
		}
	}

	function toggleCt(value: string) {
		contentTypes = toggleContentType(contentTypes, value);
	}

	// The backend clamps languages to MAX_LANGUAGES x MAX_LIST_ITEM_CHARS but returns nothing, and
	// persist() keeps this optimistic array — so without refusing here the user is shown "Collection
	// saved" for values that were silently discarded.
	let langsAtLimit = $derived(languages.length >= MAX_LANGUAGES);

	function addLang() {
		const v = langInput.trim().replace(/,$/, '').slice(0, MAX_LIST_ITEM_CHARS);
		if (v && !languages.includes(v) && !langsAtLimit) languages = [...languages, v];
		langInput = '';
	}
	function langKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ',') {
			e.preventDefault();
			addLang();
		} else if (e.key === 'Backspace' && !langInput && languages.length > 0) {
			languages = languages.slice(0, -1);
		}
	}
	function removeLang(i: number) {
		languages = languages.filter((_, idx) => idx !== i);
	}

	function formatBytes(b: number): string {
		const GB = 1073741824, MB = 1048576, KB = 1024;
		if (b >= GB) return (b / GB).toFixed(1) + ' GB';
		if (b >= MB) return (b / MB).toFixed(1) + ' MB';
		if (b >= KB) return (b / KB).toFixed(1) + ' KB';
		return b + ' B';
	}

	async function persist(): Promise<Collection> {
		// collection is non-null whenever canPublish let us in.
		const c = collection as Collection;
		const description = notes.trim() || undefined;
		await updateCollectionMeta(c.slug, description, contentTypes, tags, languages, sorted);
		const visibility: Visibility = isPrivate ? 'Private' : 'Public';
		if (visibility !== (c.visibility ?? 'Public')) {
			await updateCollectionVisibility(c.slug, visibility);
		}
		return { ...c, description, content_types: contentTypes, tags, languages, sorted, visibility };
	}

	// QURATOR-138 stands: no Save-draft — Cancel (discard), Publish (the single durable write),
	// ×/Esc (handled as Cancel by the modal). See also the QURATOR-207 note atop this file.
	async function handlePublish() {
		if (!canPublish || !collection) return;
		publishing = true;
		try {
			const updated = await persist();
			const summary = await publishCollection(collection.slug);
			onpublished?.({ ...updated, published: true });
			// devtest #7: a too-large collection publishes only a truncated paywall teaser.
			if (summary?.truncated) {
				toast(`Published a preview. Too large to publish in full, so people browsing it see ${summary.shown_items.toLocaleString()} of ${summary.total_items.toLocaleString()} items.`);
			} else {
				toast('Collection published');
			}
			close();
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			publishing = false;
		}
	}

	function close() {
		open = false;
		scanning = false;
		publishing = false;
		path = '';
		pathAlias = '';
		excludeRaw = '';
		topLevel = null;
		checked = new Set();
		treeError = '';
		loadedPath = '';
		collection = null;
		scanFolders = 0;
		contentTypes = [];
		tags = [];
		languages = [];
		langInput = '';
		notes = '';
		sorted = false;
		isPrivate = false;
		onclose?.();
	}
</script>

{#if open}
	<!-- QURATOR-97: close() wipes all run state, so the backdrop must not close — Cancel/Esc stay. -->
	<Modal open={true} width="860px" padding="0" closeOnBackdrop={false} onclose={close}>
		<div class="screen-frame">
			<!-- One header (QURATOR-206) -->
			<div class="modal-header">
				<div class="modal-title">
					{editCollection ? 'Edit collection' : 'Add collection'}{#if pathAlias}
						<span class="title-sub"> — {pathAlias}</span>{/if}
				</div>
				<button class="close-btn" aria-label="Close" onclick={close}>{@html icons.close}</button>
			</div>

			<!-- Two columns: Directory (left) + Details (right). jsdom computes no layout, so the
			     side-by-side arrangement and the ~720px collapse below are CSS-only facts that
			     tests cannot and do not claim — they pin only that both regions mount together. -->
			<div class="cols">
				<!-- LEFT: Directory -->
				<section class="col col-directory">
					<p class="col-head"><span class="step">1</span>Directory</p>

					<div class="field">
						<div class="field-label">Directory path <span class="accent-dot">•</span></div>
						<div class="path-row">
							<div class="hb-input hb-input-wrap">
								<span class="input-lead">{@html icons.folder}</span>
								<input
									class="hb-input-bare hb-mono"
									type="text"
									placeholder="C:\Movies or /mnt/data"
									bind:value={path}
								/>
							</div>
							<button class="btn-default btn-sm" onclick={browse}>Browse…</button>
						</div>
					</div>

					<div class="field">
						<label class="field-label" for="acm-alias">Display name</label>
						<input id="acm-alias" class="hb-input" type="text" placeholder="Criterion Collection" bind:value={pathAlias} />
					</div>

					<div class="field">
						<div class="field-label-row">
							<span class="field-label">Files &amp; folders to include</span>
							{#if topLevel && topLevel.length > 0}
								<div class="tree-actions">
									<button class="link-btn" type="button" onclick={selectAll}>Select all</button>
									<button class="link-btn" type="button" onclick={clearAll}>Clear</button>
								</div>
							{/if}
						</div>
						<div class="tree-box">
							{#if treeLoading}
								<div class="tree-hint">Listing folders…</div>
							{:else if treeError}
								<div class="tree-hint tree-error">{treeError}</div>
							{:else if !path}
								<div class="tree-hint">Choose a directory above, then pick which files and folders to include.</div>
							{:else if topLevel === null}
								<div class="tree-hint">
									<button class="link-btn" type="button" onclick={() => loadTopLevel(path)}>
										List folders in this directory
									</button>
								</div>
							{:else if topLevel.length === 0}
								<div class="tree-hint">This directory is empty.</div>
							{:else}
								{#each topLevel as node (node.path)}
									<ScanTreeNode {node} rel={node.name} {checked} onToggle={toggleCheck} />
								{/each}
							{/if}
						</div>
						<span class="field-hint">
							Checked folders are scanned in full; check individual files to include just those. Root-level files are always included.{#if selectedCount > 0}
								· {selectedCount} selected{/if}
						</span>
					</div>

					<div class="field">
						<div class="field-label-row">
							<label class="field-label" for="acm-exclude">Exclude patterns</label>
							<span class="field-hint">comma-separated, leave blank to include everything</span>
						</div>
						<input
							id="acm-exclude"
							class="hb-input hb-mono"
							type="text"
							placeholder=".git, node_modules, __pycache__, .DS_Store, *.tmp"
							bind:value={excludeRaw}
						/>
					</div>

					{#if collection}
						<!-- Live scan summary (mockup: "Scanned — 1,284 files · 412.6 GB in 4 selected
						     folders"). Folder count is captured at scan time, so later checkbox
						     changes don't retroactively rewrite what the run covered. -->
						<div class="scan-summary">
							<svg class="check" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path d="M20 6 9 17l-5-5"/></svg>
							Scanned — {collection.item_count.toLocaleString()} files · {formatBytes(collection.total_bytes)}{#if scanFolders > 0} in {scanFolders} selected folders{/if}
						</div>
					{/if}
					<div class="scan-actions">
						<button
							class="btn-primary btn-sm"
							onclick={handleScan}
							disabled={!path || !pathAlias || scanning}
						>
							{scanning ? 'Scanning…' : collection ? 'Rescan' : 'Start scan'}
						</button>
					</div>
				</section>

				<!-- RIGHT: Details -->
				<section class="col col-details">
					<p class="col-head"><span class="step">2</span>Details</p>

					<div class="field">
						<span class="field-label">
							Content types<HintMarker label="Content types" text="Broad categories used in search filters. Pick at least one to publish; a mixed archive can declare several." />
						</span>
						<div class="ct-row">
							{#each CONTENT_TYPES as ct (ct.value)}
								<button type="button" class="ct-toggle" class:ct-on={contentTypes.includes(ct.value)} onclick={() => toggleCt(ct.value)}>
									{ct.label}
								</button>
							{/each}
						</div>
					</div>

					<div class="field">
						<span class="field-label">Tags</span>
						<CollectionTagsEditor bind:tags />
					</div>

					<div class="field">
						<span class="field-label">Languages</span>
						<div class="hb-input lang-wrap">
							{#each languages as lang, i (lang)}
								<span class="chip">{lang}<button type="button" class="chip-x" onclick={() => removeLang(i)} aria-label={`Remove ${lang}`}>×</button></span>
							{/each}
							<input
								class="lang-input"
								type="text"
								maxlength={MAX_LIST_ITEM_CHARS}
								placeholder={langsAtLimit ? `${MAX_LANGUAGES} max` : '+ language'}
								disabled={langsAtLimit}
								bind:value={langInput}
								onkeydown={langKeydown}
							/>
						</div>
					</div>

					<div class="field">
						<label class="field-label" for="acm-notes">Notes</label>
						<textarea
							id="acm-notes"
							class="hb-input hb-textarea notes-input"
							rows="3"
							maxlength={MAX_DESCRIPTION_CHARS}
							placeholder="Add notes about this collection (visible to peers)…"
							bind:value={notes}
						></textarea>
						<!-- The notes ride in every published listing, sharing one 40 KB budget with the folder
						     tree — so the count is shown rather than letting the backend silently clamp. -->
						<div class="char-count" class:near-limit={notes.length > MAX_DESCRIPTION_CHARS - 40}>
							{notes.length}/{MAX_DESCRIPTION_CHARS}
						</div>
					</div>

					<div class="field-row">
						<label class="check-row">
							<input type="checkbox" bind:checked={sorted} />
							Sorted<HintMarker label="Sorted" text="Marks this collection as organised and curated rather than a raw dump. Shown as a badge to people browsing your listing." />
						</label>
						<label class="check-row">
							<input type="checkbox" bind:checked={isPrivate} />
							Private<HintMarker label="Private" text="Only contacts in your Private audience can open this collection. It is encrypted to each of them personally, so your share code alone won't open it. Not DRM: a recipient can still copy what they decrypt." />
						</label>
					</div>
				</section>
			</div>

			<!-- One footer (QURATOR-206): Cancel / Publish spanning both columns -->
			<div class="modal-footer">
				<span class="footer-hint">Large folders can take a few minutes to scan</span>
				<div class="footer-actions">
					<button class="btn-ghost" onclick={close}>Cancel</button>
					<button class="btn-primary" onclick={handlePublish} disabled={!canPublish || publishing}>
						{publishing ? 'Publishing…' : 'Publish'}
					</button>
				</div>
			</div>
		</div>
	</Modal>
{/if}

<style>
	/* M15 W2: backdrop/card come from Modal.svelte (padding=0); this frame keeps the header/
	   columns/footer chrome with the rounded, clipped corners the header border relies on. */
	.screen-frame { border-radius: 10px; overflow: hidden; }

	.modal-header {
		padding: 16px 20px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.modal-title { font-size: 15px; font-weight: 600; color: var(--fg); }
	.title-sub { color: var(--fg-dim); font-weight: 400; }

	.close-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-muted);
		display: flex;
		padding: 2px;
	}

	/* QURATOR-206 — the two-column body. Collapses to one stacked column at ~720px (owner
	   ruling); each column scrolls independently so the shared footer stays put. */
	.cols { display: grid; grid-template-columns: 1fr 1fr; align-items: stretch; }

	.col {
		padding: 18px 20px;
		display: flex;
		flex-direction: column;
		gap: 14px;
		min-width: 0;
		max-height: 60vh;
		overflow-y: auto;
	}

	.col-directory { border-right: 1px solid var(--border); }

	@media (max-width: 720px) {
		.cols { grid-template-columns: 1fr; }
		.col-directory { border-right: none; border-bottom: 1px solid var(--border); }
	}

	.col-head {
		margin: 0;
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.09em;
		color: var(--fg-dim);
	}

	.col-head .step {
		display: inline-grid;
		place-items: center;
		width: 17px;
		height: 17px;
		border-radius: 50%;
		background: color-mix(in oklch, var(--accent) 14%, transparent);
		color: var(--accent);
		font-size: 10px;
		font-weight: 700;
	}

	.modal-footer {
		padding: 12px 20px;
		border-top: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		background: var(--bg-elev1);
	}

	.footer-hint { font-size: 11.5px; color: var(--fg-dim); }

	.footer-actions { display: flex; gap: 8px; }

	.field { display: flex; flex-direction: column; gap: 5px; }
	.field-row { display: flex; gap: 18px; }

	.field-label {
		font-size: 11px;
		color: var(--fg-muted);
		font-weight: 500;
	}

	.field-label-row { display: flex; justify-content: space-between; align-items: baseline; }

	.field-hint { font-size: 10.5px; color: var(--fg-dim); }

	.accent-dot { color: var(--accent); margin-left: 3px; }

	.path-row { display: flex; gap: 8px; }

	/* QURATOR-101 — on the .hb-input contract; only the icon-prefix layout (gap) is local. The
	   inner input is transparent/borderless so the wrapper reads as one input field. */
	.hb-input-wrap {
		flex: 1;
		gap: 8px;
	}

	.hb-input-wrap span { color: var(--fg-dim); display: flex; }

	.hb-input-bare {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		min-width: 0;
	}

	.hb-input-bare::placeholder { color: var(--fg-dim); }

	/* Folder-tree picker */
	.tree-actions { display: flex; gap: 10px; }

	.link-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--accent);
		font-family: var(--font-ui);
		font-size: 11px;
		padding: 0;
	}

	.link-btn:hover { text-decoration: underline; }

	.tree-box {
		max-height: 280px;
		overflow-y: auto;
		padding: 8px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}

	.tree-hint { font-size: 12px; color: var(--fg-dim); padding: 4px 2px; }

	.tree-error { color: var(--error); }

	/* Live scan summary (QURATOR-206, from the approved mockup). */
	.scan-summary {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 12px;
		color: var(--fg-muted);
		font-variant-numeric: tabular-nums;
	}

	.scan-summary .check { color: var(--good); display: flex; flex: none; }

	.scan-actions { display: flex; }

	/* Details column */
	.ct-row { display: flex; flex-wrap: wrap; gap: 6px; }
	.ct-toggle {
		font-size: 11.5px;
		padding: 4px 10px;
		border-radius: 5px;
		border: 1px solid var(--border);
		background: var(--bg-elev2);
		color: var(--fg-muted);
		cursor: pointer;
		font-family: inherit;
	}
	.ct-toggle:hover { background: var(--bg-elev3); }
	.ct-on {
		background: color-mix(in oklch, var(--accent) 14%, transparent);
		color: var(--accent);
		border-color: color-mix(in oklch, var(--accent) 30%, transparent);
	}

	/* QURATOR-101 — on the .hb-input contract; height is overridden to auto (like .hb-textarea) so
	   the box can grow as chips wrap. :focus-within stands in for :focus since DOM focus lands on
	   the nested .lang-input, not this wrapper (mirrors CollectionTagsEditor's .tag-wrap). */
	.lang-wrap {
		flex-wrap: wrap;
		gap: 5px;
		height: auto;
		min-height: 34px;
		padding: 5px 8px;
	}
	.lang-wrap:focus-within { border-color: var(--accent); }
	.chip {
		display: flex;
		align-items: center;
		gap: 3px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 1px 5px 1px 7px;
		font-size: 11.5px;
		color: var(--fg);
		white-space: nowrap;
	}
	.chip-x {
		background: none; border: none; cursor: pointer; color: var(--fg-dim);
		font-size: 14px; line-height: 1; padding: 0; display: flex; align-items: center;
	}
	.chip-x:hover { color: var(--fg); }
	.lang-input {
		flex: 1;
		min-width: 60px;
		background: transparent;
		border: none;
		outline: none;
		padding: 0;
	}
	.lang-input::placeholder { color: var(--fg-dim); }

	/* QURATOR-101 — on the .hb-input/.hb-textarea contract; resize is the only local override. */
	.notes-input { resize: vertical; }
	.notes-input::placeholder { color: var(--fg-dim); }

	.char-count {
		align-self: flex-end;
		margin-top: 3px;
		font-size: 11px;
		color: var(--fg-dim);
		font-variant-numeric: tabular-nums;
	}
	.char-count.near-limit { color: var(--fg-muted); }

	.check-row {
		display: flex;
		align-items: center;
		gap: 5px;
		font-size: 12.5px;
		color: var(--fg-muted);
		cursor: pointer;
	}

	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */
</style>
