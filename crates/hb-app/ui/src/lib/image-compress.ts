//! Client-side profile-picture compression (M13 item #13) — resize + re-encode a picked image file
//! into a small WebP `data:` URI that fits inside `hb-core`'s wire cap
//! (`TEASER_PICTURE_MAX_BYTES`, 16 KB), so the picture rides inside the existing teaser body with no
//! new wire surface. Plain browser canvas APIs — no dependency, no server round-trip.

/** Mirrors `hb-core::event::TEASER_PICTURE_MAX_BYTES`. */
export const PICTURE_MAX_BYTES = 16 * 1024;

/** Compress `file` to a `data:image/webp;base64,...` URI no larger than `maxBytes`. Draws a
 *  centered-square crop to a shrinking canvas (64×64, then 32×32) and steps quality down (1.0 → 0.3)
 *  until the encoded string fits. Throws if even the smallest/lowest-quality attempt is still over
 *  the cap (caller should toast and let the user pick a simpler picture). */
export async function compressToDataUri(file: File | Blob, maxBytes: number = PICTURE_MAX_BYTES): Promise<string> {
	const bitmap = await createImageBitmap(file);
	try {
		for (const size of [64, 32]) {
			for (let quality = 1; quality >= 0.3; quality -= 0.1) {
				const uri = drawToDataUri(bitmap, size, quality);
				// The cap is checked against the wire form itself (the whole data: URI string, ASCII-only
				// — .length is the byte count), matching hb-core's `Teaser::picture` validation exactly.
				if (uri.length <= maxBytes) return uri;
			}
		}
		throw new Error(`Could not compress this image under ${maxBytes} bytes — try a simpler picture.`);
	} finally {
		bitmap.close();
	}
}

function drawToDataUri(bitmap: ImageBitmap, size: number, quality: number): string {
	const canvas = document.createElement('canvas');
	canvas.width = size;
	canvas.height = size;
	const ctx = canvas.getContext('2d');
	if (!ctx) throw new Error('Canvas is not available in this environment');
	// Cover-fit: crop to a centered square before scaling down.
	const side = Math.min(bitmap.width, bitmap.height);
	const sx = (bitmap.width - side) / 2;
	const sy = (bitmap.height - side) / 2;
	ctx.drawImage(bitmap, sx, sy, side, side, 0, 0, size, size);
	return canvas.toDataURL('image/webp', quality);
}
