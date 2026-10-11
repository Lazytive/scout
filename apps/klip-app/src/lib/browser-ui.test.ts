import { describe, expect, it } from 'vitest';

import { blockBrowserUi, browserKey, textField } from './browser-ui';

const key = (k: string, mods: Partial<{ ctrl: boolean; shift: boolean; alt: boolean }> = {}) => ({
  key: k,
  ctrlKey: !!mods.ctrl,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
  metaKey: false,
});

describe('browser UI', () => {
  it('cancels the browser accelerators', () => {
    for (const k of ['p', 'S', 'f', 'g', 'r', 'u', 'j', 'h', 'o']) expect(browserKey(key(k, { ctrl: true })), `Ctrl+${k}`).toBe(true);
    for (const k of ['I', 'J', 'C', 'R']) expect(browserKey(key(k, { ctrl: true, shift: true })), `Ctrl+Shift+${k}`).toBe(true);
    for (const k of ['F3', 'F5', 'F7', 'F12']) expect(browserKey(key(k)), k).toBe(true);
    expect(browserKey(key('ArrowLeft', { alt: true }))).toBe(true);
  });

  it('leaves Scout’s own keys alone', () => {
    for (const k of ['z', 'y', 'k', 'a']) expect(browserKey(key(k, { ctrl: true })), `Ctrl+${k}`).toBe(false);
    expect(browserKey(key('Z', { ctrl: true, shift: true }))).toBe(false);
    for (const k of ['p', 's', 'f', ' ', 'Delete', 'ArrowLeft', 'F2']) expect(browserKey(key(k)), k).toBe(false);
    expect(browserKey(key('ArrowLeft', { shift: true }))).toBe(false);
    // AltGr (Ctrl+Alt) types characters on some layouts.
    expect(browserKey(key('s', { ctrl: true, alt: true }))).toBe(false);
  });

  it('keeps the context menu in text fields only', () => {
    const at = (el: { tagName: string; type?: string } | null) => ({ closest: () => el });
    expect(textField(at({ tagName: 'INPUT', type: 'text' }) as unknown as EventTarget)).toBe(true);
    expect(textField(at({ tagName: 'INPUT', type: 'search' }) as unknown as EventTarget)).toBe(true);
    expect(textField(at({ tagName: 'TEXTAREA' }) as unknown as EventTarget)).toBe(true);
    expect(textField(at({ tagName: 'INPUT', type: 'range' }) as unknown as EventTarget)).toBe(false);
    expect(textField(at(null) as unknown as EventTarget)).toBe(false);
    expect(textField(null)).toBe(false);
  });

  it('cancels the defaults in the page, nothing else', () => {
    const win = new EventTarget();
    const stop = blockBrowserUi(win as unknown as Window);
    const ev = (type: string, init: Record<string, unknown>) => Object.assign(new Event(type, { cancelable: true }), init);
    const print = ev('keydown', key('p', { ctrl: true }));
    const undo = ev('keydown', key('z', { ctrl: true }));
    const menu = ev('contextmenu', {});
    let seen = 0;
    win.addEventListener('keydown', () => seen++);
    win.dispatchEvent(print);
    win.dispatchEvent(undo);
    win.dispatchEvent(menu);
    expect([print.defaultPrevented, undo.defaultPrevented, menu.defaultPrevented]).toEqual([true, false, true]);
    expect(seen).toBe(2);
    stop();
    const again = ev('keydown', key('p', { ctrl: true }));
    win.dispatchEvent(again);
    expect(again.defaultPrevented).toBe(false);
  });
});
