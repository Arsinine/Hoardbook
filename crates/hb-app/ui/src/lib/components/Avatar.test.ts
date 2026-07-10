// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import Avatar from './Avatar.svelte';

afterEach(cleanup);

describe('Avatar — letter fallback vs data: picture (M13 item #13)', () => {
	it('renders the letter div when no picture is given', () => {
		const { container } = render(Avatar, { props: { letter: 'T' } });
		expect(container.querySelector('img')).toBeNull();
		expect(container.textContent).toContain('T');
	});

	it('renders an <img> for a data: picture', () => {
		const { container } = render(Avatar, {
			props: { letter: 'T', picture: 'data:image/webp;base64,AAAA' },
		});
		const img = container.querySelector('img');
		expect(img).not.toBeNull();
		expect(img!.getAttribute('src')).toBe('data:image/webp;base64,AAAA');
	});

	it('falls back to the letter div for a non-data: picture (defense in depth)', () => {
		const { container } = render(Avatar, {
			props: { letter: 'T', picture: 'https://evil.example/track.png' },
		});
		expect(container.querySelector('img')).toBeNull();
		expect(container.textContent).toContain('T');
	});
});
