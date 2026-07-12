// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from 'vitest';
import { focusableWithin, nextFocus } from './modal-focus.js';

function mount(html: string): HTMLElement {
	const root = document.createElement('div');
	root.innerHTML = html;
	document.body.appendChild(root);
	return root;
}

describe('modal-focus — focusableWithin', () => {
	beforeEach(() => { document.body.innerHTML = ''; });

	it('collects interactive elements in DOM order', () => {
		const root = mount(`
			<button id="a">a</button>
			<input id="b" />
			<a id="c" href="#">c</a>
			<textarea id="d"></textarea>
		`);
		expect(focusableWithin(root).map((e) => e.id)).toEqual(['a', 'b', 'c', 'd']);
	});

	it('excludes disabled controls and tabindex="-1"', () => {
		const root = mount(`
			<button id="a">a</button>
			<button id="b" disabled>b</button>
			<div id="c" tabindex="-1">c</div>
			<div id="d" tabindex="0">d</div>
		`);
		expect(focusableWithin(root).map((e) => e.id)).toEqual(['a', 'd']);
	});

	it('an anchor without href is not focusable', () => {
		const root = mount(`<a id="a">no href</a><a id="b" href="#">yes</a>`);
		expect(focusableWithin(root).map((e) => e.id)).toEqual(['b']);
	});
});

describe('modal-focus — nextFocus', () => {
	function list(n: number): HTMLElement[] {
		return Array.from({ length: n }, (_, i) => {
			const el = document.createElement('button');
			el.id = String(i);
			return el;
		});
	}

	it('returns null for an empty list', () => {
		expect(nextFocus([], null, false)).toBeNull();
	});

	it('advances forward and wraps at the end', () => {
		const l = list(3);
		expect(nextFocus(l, l[0], false)).toBe(l[1]);
		expect(nextFocus(l, l[2], false)).toBe(l[0]); // wrap
	});

	it('advances backward and wraps at the start', () => {
		const l = list(3);
		expect(nextFocus(l, l[2], true)).toBe(l[1]);
		expect(nextFocus(l, l[0], true)).toBe(l[2]); // wrap
	});

	it('lands on first (Tab) / last (Shift+Tab) when active is outside the list', () => {
		const l = list(3);
		expect(nextFocus(l, null, false)).toBe(l[0]);
		expect(nextFocus(l, null, true)).toBe(l[2]);
	});
});
