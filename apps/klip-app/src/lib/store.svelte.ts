// App state: the engine's status (refreshed on events and every few seconds while the
// window is open) and a short activity log.

import { call, onEvent } from './api';
import { describeEvent, type Tone } from './format';
import type { KlipEvent, Status, StorageStatus } from './types';

export interface Activity {
  id: number;
  at: Date;
  text: string;
  tone: Tone;
}

const MAX_ACTIVITY = 50;

export const app = $state({
  status: null as Status | null,
  storage: null as StorageStatus | null,
  activity: [] as Activity[],
  /** The last command error, shown until the next command. */
  error: null as string | null,
  busy: false,
});

let nextId = 0;

function log(e: KlipEvent) {
  const d = describeEvent(e);
  if (!d) return;
  nextId += 1;
  app.activity = [{ id: nextId, at: new Date(), ...d }, ...app.activity].slice(0, MAX_ACTIVITY);
}

export async function refresh(): Promise<void> {
  try {
    app.status = await call<Status>('engine.status');
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
  }
}

export async function refreshStorage(): Promise<void> {
  try {
    app.storage = await call<StorageStatus>('storage.status');
  } catch {
    app.storage = null;
  }
}

/** Run a command for a button; errors end up in `app.error`. */
export async function run(cmd: string, args?: unknown): Promise<void> {
  app.busy = true;
  app.error = null;
  try {
    await call(cmd, args);
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
  } finally {
    app.busy = false;
    await refresh();
  }
}

/** Start listening; returns a stop function. */
export async function connect(): Promise<() => void> {
  const unlisten = await onEvent((e) => {
    log(e);
    void refresh();
    if (e.event === 'clip_saved' || e.event === 'storage_cleaned' || e.event === 'settings_changed') {
      void refreshStorage();
    }
  });
  await Promise.all([refresh(), refreshStorage()]);
  let ticks = 0;
  const timer = setInterval(() => {
    if (document.visibilityState !== 'visible') return;
    ticks += 1;
    void refresh();
    if (ticks % 10 === 0) void refreshStorage();
  }, 2000);
  return () => {
    unlisten();
    clearInterval(timer);
  };
}
