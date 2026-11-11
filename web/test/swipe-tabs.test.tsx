// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { useSwipeTabs, type SwipeDirection } from '../src/shared/ui/use-swipe-tabs.ts';

/**
 * Swipe between tabs. The contract is what a phone does with a finger on a page:
 * a clearly horizontal touch swipe moves one tab, and everything else — a mouse,
 * a scroll the browser took over, a drag on a control or a sideways row — is not
 * the page's to reinterpret.
 */
function pointer(type: string, init: PointerEventInit): PointerEvent {
  return new window.PointerEvent(type, { bubbles: true, composed: true, cancelable: true, pointerType: 'touch', isPrimary: true, pointerId: 1, ...init });
}

let root: Root | null = null;
afterEach(() => { act(() => root?.unmount()); root = null; document.body.replaceChildren(); });

function mount(enabled = true) {
  const seen: SwipeDirection[] = [];
  function Page() {
    const ref = useSwipeTabs<HTMLDivElement>(enabled ? direction => { seen.push(direction); return true; } : null);
    return <div ref={ref} id="page"><input id="field" /><div id="row" style={{ overflowX: 'auto' }}><a id="card" href="#x">card</a></div><p id="text">text</p></div>;
  }
  const host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
  act(() => root!.render(<Page />));
  return { seen, page: document.getElementById('page')!, at: (id: string) => document.getElementById(id)! };
}

function swipe(target: Element, dx: number, dy = 0, type: 'up' | 'cancel' = 'up', pointerType = 'touch') {
  target.dispatchEvent(pointer('pointerdown', { clientX: 200, clientY: 300, pointerType }));
  target.dispatchEvent(pointer(type === 'up' ? 'pointerup' : 'pointercancel', { clientX: 200 + dx, clientY: 300 + dy, pointerType }));
}

describe('useSwipeTabs', () => {
  it('turns a left swipe into next and a right swipe into previous', () => {
    const { seen, at } = mount();
    swipe(at('text'), -120);
    swipe(at('text'), 120);
    expect(seen).toEqual(['next', 'previous']);
  });

  it('gives up horizontal panning only where it is enabled', () => {
    expect(mount().page.classList.contains('swipe-tabs')).toBe(true);
    act(() => root?.unmount()); root = null; document.body.replaceChildren();
    expect(mount(false).page.classList.contains('swipe-tabs')).toBe(false);
  });

  it('ignores a mouse, a short move, a diagonal and a scroll the browser took', () => {
    const { seen, at } = mount();
    swipe(at('text'), -120, 0, 'up', 'mouse');
    swipe(at('text'), -40);
    swipe(at('text'), -120, 90);
    swipe(at('text'), -120, 0, 'cancel');
    expect(seen).toEqual([]);
  });

  it('leaves a control and a sideways-scrolling row alone', () => {
    const { seen, at } = mount();
    const row = at('row');
    Object.defineProperty(row, 'scrollWidth', { get: () => 900 });
    Object.defineProperty(row, 'clientWidth', { get: () => 300 });
    swipe(at('field'), -120);
    swipe(at('card'), -120);
    expect(seen).toEqual([]);
  });

  it('does not let the swipe click the card the finger lifted over', () => {
    const { seen, at } = mount();
    swipe(at('card'), -120);
    const click = new window.MouseEvent('click', { bubbles: true, cancelable: true });
    at('card').dispatchEvent(click);
    expect(seen).toEqual(['next']);
    expect(click.defaultPrevented).toBe(true);
  });
});

