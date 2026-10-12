// Scout's own title bar on Windows (M11.3): the system title bar is off, so the app
// draws the minimize / maximize / close buttons and lets the empty parts of its bar move
// the window (double-click: maximize). The app turns it on with `__KLIP_CHROME__`; on
// Linux the system title bar stays. The browser mock shows the buttons with `?chrome`
// (they do nothing there).

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export const customChrome =
  typeof window !== 'undefined' &&
  ((window as { __KLIP_CHROME__?: boolean }).__KLIP_CHROME__ === true ||
    (!inTauri && new URLSearchParams(window.location.search).has('chrome')));

/** What a press may land on without moving the window: controls and pop-ups. */
const CONTROLS =
  'button, a, input, select, textarea, label, [role="dialog"], [role="menu"], [role="listbox"], [data-no-drag]';

/** Whether a press on the title bar at `target` moves the window (not on a control). */
export function dragsWindow(target: { closest(selector: string): unknown } | null): boolean {
  return !!target && !target.closest(CONTROLS);
}

async function current() {
  if (!inTauri) return null;
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  return getCurrentWindow();
}

/** Run a window action; a refused or failed one leaves the window as it is. */
async function act(f: (w: NonNullable<Awaited<ReturnType<typeof current>>>) => Promise<void>): Promise<void> {
  try {
    const w = await current();
    if (w) await f(w);
  } catch {
    // Nothing depends on it.
  }
}

export const minimize = () => act((w) => w.minimize());
export const toggleMaximize = () => act((w) => w.toggleMaximize());
/** Like the system's close button: Scout's close handling decides (tray or quit). */
export const closeWindow = () => act((w) => w.close());
export const startDrag = () => act((w) => w.startDragging());

/** Follow the maximized state (the middle button's icon); returns a stop function. */
export async function watchMaximized(cb: (maximized: boolean) => void): Promise<() => void> {
  try {
    const w = await current();
    if (!w) return () => undefined;
    cb(await w.isMaximized());
    return await w.onResized(() => {
      void w.isMaximized().then(cb, () => undefined);
    });
  } catch {
    return () => undefined;
  }
}
