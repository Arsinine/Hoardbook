<script lang="ts">
	// Two-step Add-collection wizard (M13 W5 Slice 1): Step 1 "Source" reuses ScanDialog to pick a
	// folder; Step 2 "Details" is CollectionDetailsForm. Also doubles as the "Edit details" reopen
	// (pass `editCollection` to jump straight to step 2 for an already-scanned collection).
	import ScanDialog from './ScanDialog.svelte';
	import CollectionDetailsForm from './CollectionDetailsForm.svelte';
	import Modal from './Modal.svelte';
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
		<Modal open={true} width="460px" padding="0" onclose={close}>
			<CollectionDetailsForm {collection} onsaved={handleSaved} onpublished={handlePublished} oncancel={close} />
		</Modal>
	{/if}
{/if}
<!-- M15 W2: step-1 uses ScanDialog (itself on Modal); step-2 wraps the details form in Modal. -->
