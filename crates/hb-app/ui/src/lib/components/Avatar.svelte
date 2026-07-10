<script lang="ts">
	import { avatarHue } from '$lib/icons.js';

	interface Props {
		letter?: string;
		size?: number;
		hue?: number | undefined;
		/** Optional avatar as a `data:` URI (M13 item #13). Defense-in-depth: only a `data:` URI ever
		 *  renders as an `<img>` — an `http(s)` value (which should never reach here; hb-core rejects
		 *  it on publish and sanitizes it on parse) falls back to the letter avatar instead of fetching. */
		picture?: string;
	}

	let { letter = '?', size = 36, hue = undefined, picture = undefined }: Props = $props();

	let h = $derived(hue ?? avatarHue(letter));
	let grad = $derived(`linear-gradient(135deg, oklch(0.55 0.10 ${h}) 0%, oklch(0.40 0.08 ${h + 40}) 100%)`);
	let br = $derived(size > 28 ? '8px' : '6px');
	let fs = $derived(`${(size * 0.42).toFixed(1)}px`);
	let isPicture = $derived(picture?.startsWith('data:') ?? false);
</script>

{#if isPicture}
	<img
		src={picture}
		alt=""
		width={size}
		height={size}
		style="
			border-radius:{br}; flex-shrink:0; object-fit:cover;
			box-shadow:inset 0 0 0 1px oklch(1 0 0 / 0.08);
		"
	/>
{:else}
	<div style="
		width:{size}px; height:{size}px;
		border-radius:{br};
		background:{grad};
		color:oklch(0.98 0 0);
		display:flex; align-items:center; justify-content:center;
		font-weight:700; font-family:var(--font-ui);
		font-size:{fs}; flex-shrink:0; letter-spacing:-0.5px;
		box-shadow:inset 0 0 0 1px oklch(1 0 0 / 0.08);
	">{letter.toUpperCase()}</div>
{/if}
