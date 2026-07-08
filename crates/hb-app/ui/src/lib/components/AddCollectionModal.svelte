<script lang="ts">
	// Two-step Add-collection wizard (M13 W5 Slice 1): Step 1 "Source" reuses ScanDialog to pick a
	// folder; Step 2 "Details" is CollectionDetailsForm. Also doubles as the "Edit details" reopen
	// (pass `editCollection` to jump straight to step 2 for an already-scanned collection).
	import ScanDialog from './ScanDialog.svelte';
	import CollectionDetailsForm from './CollectionDetailsForm.svelte';
	import type { Collection } from '../types.js';

	interface Props {
		open?: boolean;
		/** When set, the wizard skips the Source step and opens straight into Details for this collection. */
		editCollection?: Collection | null;
		onscanned?: (collection: Collection) => void;
		onsaved?: (collection: Collection) => void;
		onpublished?: (collection: Collection) => void;
		onclose?: () => void;
	}

	let {
		open = $bindable(false),
		editCollection = null,
		onscanned,
		onsaved,
		onpublished,
		onclose
	}: Props = $props();

	let step: 1 | 2 = $state(1);
	let collection: Collection | null = $state(null);

	// Re-seed step/collection on every closed→open transition (mirrors ScanDialog's own `wasOpen`
	// convention) so a stale collection never leaks into the next "Add collection" run.
	// Not reactive on purpose — a plain transition-edge flag, never read by the template, so it
	// mustn't be part of the effect's own dependency tracking (avoids a self-triggering effect).
	let wasOpen = false;
	$effect(() => {
		if (open && !wasOpen) {
			wasOpen = true;
			if (editCollection) {
				collection = editCollection;
				step = 2;
			} else {
				collection = null;
				step = 1;
			}
		} else if (!open && wasOpen) {
			wasOpen = false;
		}
	});

	function handleScanned(scanned: Collection) {
		collection = scanned;
		step = 2;
		onscanned?.(scanned);
	}

	function handleScanDialogClose() {
		// ScanDialog closed (Cancel/Escape) without producing a scan — close the whole wizard too.
		if (step === 1) close();
	}

	function handleSaved(updated: Collection) {
		onsaved?.(updated);
		close();
	}

	function handlePublished(updated: Collection) {
		onpublished?.(updated);
		close();
	}

	function close() {
		open = false;
		step = 1;
		collection = null;
		onclose?.();
	}
</script>

{#if open}
	{#if step === 1}
		<ScanDialog open={true} title="Add collection" onscanned={handleScanned} onclose={handleScanDialogClose} />
	{:else if step === 2 && collection}
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="backdrop" onclick={(e) => { if (e.target === e.currentTarget) close(); }} onkeydown={(e) => e.key === 'Escape' && close()} role="presentation">
			<div class="modal">
				<CollectionDetailsForm {collection} onsaved={handleSaved} onpublished={handlePublished} oncancel={close} />
			</div>
		</div>
	{/if}
{/if}

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: oklch(0.10 0.005 260 / 0.7);
		backdrop-filter: blur(4px);
		z-index: 100;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 30px;
	}

	.modal {
		width: 460px;
		max-width: calc(100vw - 40px);
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 10px;
		box-shadow: 0 30px 80px -20px oklch(0 0 0 / 0.7), 0 0 0 1px oklch(1 0 0 / 0.06);
		overflow: hidden;
	}
</style>
