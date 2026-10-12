// A short message at the bottom of the window ("Copied: paste it anywhere"); one at a
// time, the newest replaces the last (M11.4).

export interface Toast {
  id: number;
  text: string;
  kind: 'ok' | 'error';
}

export const toasts = $state<{ current: Toast | null }>({ current: null });

let seq = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

/** Show `text` for `ms` (errors stay a little longer). */
export function toast(text: string, kind: Toast['kind'] = 'ok', ms = kind === 'error' ? 6000 : 3500): void {
  seq += 1;
  toasts.current = { id: seq, text, kind };
  clearTimeout(timer);
  timer = setTimeout(() => (toasts.current = null), ms);
}

export function dismissToast(): void {
  clearTimeout(timer);
  toasts.current = null;
}
