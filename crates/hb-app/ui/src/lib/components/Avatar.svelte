<script lang="ts">
	import { avatarHue } from '$lib/icons.js';

	interface Props {
		letter?: string;
		size?: number;
		hue?: number | undefined;
	}

	let { letter = '?', size = 36, hue = undefined }: Props = $props();

	let h = $derived(hue ?? avatarHue(letter));
	let grad = $derived(`linear-gradient(135deg, oklch(0.55 0.10 ${h}) 0%, oklch(0.40 0.08 ${h + 40}) 100%)`);
	let br = $derived(size > 28 ? '8px' : '6px');
	let fs = $derived(`${(size * 0.42).toFixed(1)}px`);
</script>

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
