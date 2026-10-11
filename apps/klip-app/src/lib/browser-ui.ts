// No browser UI in the app (M9.8.1): the WebView's own context menu (Print, Save as,
// Inspect…) and its accelerators (Ctrl+P print, Ctrl+S save as, Ctrl+F find, F5 reload…)
// belong to a web browser, not to Klip. Their default actions are cancelled in the page;
// Klip's own handlers still see the keys (Ctrl+Z, Ctrl+K…), and text fields keep their
// context menu (copy / paste).

export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** Ctrl + these: print, save as, find (and next), reload, view source, downloads, history, open. */
const CTRL = new Set(['p', 's', 'f', 'g', 'r', 'u', 'j', 'h', 'o']);
/** Ctrl+Shift + these: the developer tools. */
const CTRL_SHIFT = new Set(['i', 'j', 'c']);
/** Alone: find next, reload, caret browsing, developer tools. */
const KEYS = new Set(['F3', 'F5', 'F7', 'F12', 'BrowserBack', 'BrowserForward', 'BrowserRefresh', 'BrowserSearch']);

/** Whether a key press would open browser UI (its default action is to be cancelled). */
export function browserKey(e: KeyLike): boolean {
  if (KEYS.has(e.key)) return true;
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const ctrl = e.ctrlKey || e.metaKey;
  // Alt+← / Alt+→: back and forward.
  if (e.altKey && !ctrl && (k === 'ArrowLeft' || k === 'ArrowRight')) return true;
  if (!ctrl || e.altKey) return false;
  return e.shiftKey ? CTRL_SHIFT.has(k) || k === 'r' : CTRL.has(k);
}

/** Where the browser's context menu stays: editable text (copy, paste, spelling). */
export function textField(target: EventTarget | null): boolean {
  const t = target as { closest?: (sel: string) => { tagName: string; type?: string } | null } | null;
  if (typeof t?.closest !== 'function') return false;
  const el = t.closest('input, textarea, [contenteditable=""], [contenteditable="true"]');
  if (!el) return false;
  if (el.tagName === 'INPUT') return !NOT_TEXT.has((el.type ?? 'text').toLowerCase());
  return true;
}
const NOT_TEXT = new Set(['button', 'checkbox', 'radio', 'range', 'color', 'file', 'submit', 'reset', 'image']);

/** Capture: before anything that might stop the event on its way down. */
const CAPTURE = { capture: true };

/** Cancel browser UI in this window (call once at start-up). */
export function blockBrowserUi(win: Window = window): () => void {
  const onKey = (e: KeyboardEvent) => {
    if (browserKey(e)) e.preventDefault();
  };
  const onMenu = (e: MouseEvent) => {
    if (!textField(e.target)) e.preventDefault();
  };
  win.addEventListener('keydown', onKey, CAPTURE);
  win.addEventListener('contextmenu', onMenu, CAPTURE);
  return () => {
    win.removeEventListener('keydown', onKey, CAPTURE);
    win.removeEventListener('contextmenu', onMenu, CAPTURE);
  };
}
