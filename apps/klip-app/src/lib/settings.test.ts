import { describe, expect, it } from 'vitest';

import { bindingError, captureKey, keyName, keyParts, patchFor, splitError, type KeyLike } from './settings';

const key = (code: string, mods: Partial<KeyLike> = {}): KeyLike => ({
  code,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

describe('patchFor', () => {
  it('nests the value under the path', () => {
    expect(patchFor(['recording', 'buffer_secs'], 60)).toEqual({ recording: { buffer_secs: 60 } });
    expect(patchFor(['games', 'profiles', 'cs2.exe'], null)).toEqual({ games: { profiles: { 'cs2.exe': null } } });
    expect(patchFor(['hotkeys'], [])).toEqual({ hotkeys: [] });
  });
});

describe('splitError', () => {
  it('separates the setting from the message', () => {
    expect(splitError('recording.buffer_secs: 2 is outside 5–1800')).toEqual({
      path: 'recording.buffer_secs',
      text: '2 is outside 5–1800',
    });
    expect(splitError('hotkeys[1].keys: Alt+Q is already used by another action')).toEqual({
      path: 'hotkeys[1].keys',
      text: 'Alt+Q is already used by another action',
    });
    expect(splitError('saving the settings: disk full')).toEqual({
      path: '',
      text: 'saving the settings: disk full',
    });
  });
});

describe('hotkey capture', () => {
  it('names physical keys like the engine does', () => {
    expect(keyName('KeyA')).toBe('A');
    expect(keyName('Digit7')).toBe('7');
    expect(keyName('F10')).toBe('F10');
    expect(keyName('F24')).toBe('F24');
    expect(keyName('F25')).toBeNull();
    expect(keyName('Numpad3')).toBe('Num3');
    expect(keyName('NumpadAdd')).toBe('NumAdd');
    expect(keyName('ArrowLeft')).toBe('Left');
    expect(keyName('Backquote')).toBe('`');
    expect(keyName('IntlBackslash')).toBeNull();
  });
  it('builds combinations in the stored order', () => {
    expect(captureKey(key('F10', { altKey: true }))).toEqual({ kind: 'combo', keys: 'Alt+F10', bare: false });
    expect(captureKey(key('KeyS', { shiftKey: true, ctrlKey: true, metaKey: true, altKey: true }))).toEqual({
      kind: 'combo',
      keys: 'Ctrl+Alt+Shift+Win+S',
      bare: false,
    });
    expect(captureKey(key('F13'))).toEqual({ kind: 'combo', keys: 'F13', bare: true });
  });
  it('waits, cancels and clears', () => {
    expect(captureKey(key('AltLeft', { altKey: true }))).toEqual({ kind: 'wait' });
    expect(captureKey(key('Escape'))).toEqual({ kind: 'cancel' });
    expect(captureKey(key('Backspace'))).toEqual({ kind: 'clear' });
    expect(captureKey(key('Delete', { ctrlKey: true }))).toEqual({ kind: 'combo', keys: 'Ctrl+Delete', bare: false });
    expect(captureKey(key('IntlRo')).kind).toBe('unsupported');
  });
  it('splits keys and finds engine errors', () => {
    expect(keyParts('Alt+Shift+F10')).toEqual(['Alt', 'Shift', 'F10']);
    expect(keyParts('')).toEqual([]);
    const bound = [
      { keys: 'Alt+F10', error: 'already used by another app' },
      { keys: 'Alt+F8', error: null },
    ];
    expect(bindingError('Alt+F10', bound)).toBe('already used by another app');
    expect(bindingError('Alt+F8', bound)).toBeNull();
    expect(bindingError('', bound)).toBeNull();
  });
});
