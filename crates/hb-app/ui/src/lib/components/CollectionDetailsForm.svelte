<script lang="ts">
	// Step 2 ("Details") of the Add-collection wizard (M13 W5 Slice 1) — also reopened standalone from
	// a CollectionRow's "Edit details" menu action. Content types, tags, languages, notes, Sorted and
	// Private. Publish is gated on ≥1 content type here (a collection is otherwise publish-ready the
	// moment it exists — this replaces the old always-on per-row warning).
	import type { Collection, Visibility } from '../types.js';
	import { toggleContentType } from '../content-types.js';
	import { updateCollectionMeta, updateCollectionVisibility, publishCollection } from '../api.js';
	import { toast } from '../stores.js';
	import HintMarker from './HintMarker.svelte';
	import CollectionTagsEditor from './CollectionTagsEditor.svelte';

	interface Props {
		collection: Collection;
		onsaved?: (collection: Collection) => void;
		onpublished?: (collection: Collection) => void;
		oncancel?: () => void;
	}

	let { collection, onsaved, onpublished, oncancel }: Props = $props();

	// Same fixed six-value enum as the profile/discover pickers (HOARDBOOK_SPEC §4).
	const CONTENT_TYPES: { value: string; label: string }[] = [
		{ value: 'video', label: 'Video' },
		{ value: 'audio', label: 'Audio' },
		{ value: 'image', label: 'Image' },
		{ value: 'text', label: 'Text' },
		{ value: 'software', label: 'Software' },
		{ value: 'other', label: 'Other' },
	];

	let contentTypes: string[] = $state([]);
	let tags: string[] = $state([]);
	let languages: string[] = $state([]);
	let langInput = $state('');
	let notes = $state('');
	let sorted = $state(false);
	let isPrivate = $state(false);
	let saving = $state(false);
	let publishing = $state(false);

	// Seed the editable fields whenever a different collection is handed in (fresh scan or reopen).
	let loadedSlug = $state('');
	$effect(() => {
		if (collection.slug !== loadedSlug) {
			loadedSlug = collection.slug;
			contentTypes = [...(collection.content_types ?? [])];
			tags = [...(collection.tags ?? [])];
			languages = [...(collection.languages ?? [])];
			notes = collection.description ?? '';
			sorted = collection.sorted ?? false;
			isPrivate = (collection.visibility ?? 'Public') === 'Private';
		}
	});

	let canPublish = $derived(contentTypes.length > 0);

	function toggleCt(value: string) {
		contentTypes = toggleContentType(contentTypes, value);
	}

	function addLang() {
		const v = langInput.trim().replace(/,$/, '');
		if (v && !languages.includes(v)) languages = [...languages, v];
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

	async function persist(): Promise<Collection> {
		const description = notes.trim() || undefined;
		await updateCollectionMeta(collection.slug, description, contentTypes, tags, languages, sorted);
		const visibility: Visibility = isPrivate ? 'Private' : 'Public';
		if (visibility !== (collection.visibility ?? 'Public')) {
			await updateCollectionVisibility(collection.slug, visibility);
		}
		return { ...collection, description, content_types: contentTypes, tags, languages, sorted, visibility };
	}

	async function handleSaveDraft() {
		saving = true;
		try {
			const updated = await persist();
			onsaved?.(updated);
			toast('Collection saved');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			saving = false;
		}
	}

	async function handlePublish() {
		if (!canPublish) return;
		publishing = true;
		try {
			const updated = await persist();
			await publishCollection(collection.slug);
			onpublished?.({ ...updated, published: true });
			toast('Collection published');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			publishing = false;
		}
	}
</script>

<div class="modal-header">
	<div class="modal-title">{collection.path_alias}</div>
	<button type="button" class="close-btn" onclick={() => oncancel?.()}>×</button>
</div>

<div class="modal-body">
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
		<div class="lang-wrap">
			{#each languages as lang, i (lang)}
				<span class="chip">{lang}<button type="button" class="chip-x" onclick={() => removeLang(i)} aria-label={`Remove ${lang}`}>×</button></span>
			{/each}
			<input class="lang-input" type="text" placeholder="+ language" bind:value={langInput} onkeydown={langKeydown} />
		</div>
	</div>

	<div class="field">
		<label class="field-label" for="cdf-notes">Notes</label>
		<textarea id="cdf-notes" class="notes-input" rows="3" placeholder="Add notes about this collection (visible to peers)…" bind:value={notes}></textarea>
	</div>

	<div class="field-row">
		<label class="check-row">
			<input type="checkbox" bind:checked={sorted} />
			Sorted<HintMarker label="Sorted" text="Marks this collection as organised and curated rather than a raw dump. Shown as a badge to people browsing your listing." />
		</label>
		<label class="check-row">
			<input type="checkbox" bind:checked={isPrivate} />
			Private<HintMarker label="Private" text="Only contacts in your trusted groups can open this collection — it is encrypted to each of them personally, so your share code alone won't open it. Not DRM: a trusted contact can still copy what they decrypt." />
		</label>
	</div>
</div>

<div class="modal-footer">
	<button type="button" class="btn-ghost" onclick={() => oncancel?.()}>Cancel</button>
	<div class="footer-actions">
		<button type="button" class="btn-ghost" onclick={handleSaveDraft} disabled={saving}>
			{saving ? 'Saving…' : 'Save draft'}
		</button>
		<button type="button" class="btn-primary" onclick={handlePublish} disabled={!canPublish || publishing}>
			{publishing ? 'Publishing…' : 'Publish'}
		</button>
	</div>
</div>

<style>
	.modal-header {
		padding: 16px 20px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.modal-title { font-size: 15px; font-weight: 600; color: var(--fg); }
	.close-btn {
		background: transparent; border: none; cursor: pointer; color: var(--fg-muted);
		font-size: 18px; line-height: 1; padding: 2px;
	}

	.modal-body { padding: 20px; display: flex; flex-direction: column; gap: 14px; max-height: 60vh; overflow-y: auto; }

	.modal-footer {
		padding: 12px 20px;
		border-top: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
		background: var(--bg-elev1);
	}
	.footer-actions { display: flex; gap: 8px; }

	.field { display: flex; flex-direction: column; gap: 6px; }
	.field-row { display: flex; gap: 18px; }

	.field-label {
		display: inline-flex;
		align-items: center;
		font-size: 11px;
		color: var(--fg-muted);
		font-weight: 500;
	}

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

	.lang-wrap, .chip {
		display: flex;
	}
	.lang-wrap {
		flex-wrap: wrap;
		gap: 5px;
		min-height: 34px;
		padding: 5px 8px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
		align-items: center;
	}
	.chip {
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
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		padding: 0;
	}
	.lang-input::placeholder { color: var(--fg-dim); }

	.notes-input {
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
		padding: 9px 11px;
		font-family: var(--font-ui);
		font-size: 12.5px;
		color: var(--fg);
		resize: vertical;
	}
	.notes-input::placeholder { color: var(--fg-dim); }

	.check-row {
		display: flex;
		align-items: center;
		gap: 5px;
		font-size: 12.5px;
		color: var(--fg-muted);
		cursor: pointer;
	}

	.btn-primary {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 7px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; white-space: nowrap; line-height: 1;
	}
	.btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

	.btn-ghost {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 7px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg-muted); background: transparent;
		border: 1px solid transparent; border-radius: 7px;
		cursor: pointer; white-space: nowrap; line-height: 1;
	}
	.btn-ghost:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
