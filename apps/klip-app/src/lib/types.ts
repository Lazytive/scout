// Shapes of what the engine returns and emits (klip-engine `engine.status`, `Event`).

export interface Game {
  pid: number;
  exe: string;
  key: string;
  name: string;
  window: number | null;
  source: { kind: 'user' } | { kind: 'library'; store: string };
  foreground: boolean;
}

export interface HotkeyState {
  id: string;
  action: string | null;
  keys: string | null;
  secs: number | null;
  trigger: string;
  error: string | null;
}

export interface RecorderStatus {
  encoder?: string;
  width?: number;
  height?: number;
  fps?: number;
  buffer_ms?: number;
  buffered_ms?: number;
  bytes?: number;
  max_bytes?: number;
  uptime_ms?: number;
}

export interface Status {
  buffer_mode: 'games' | 'always' | 'off';
  game: Game | null;
  recording: boolean;
  desktop: boolean;
  starting: boolean;
  clip_dir: string | null;
  session: boolean | null;
  recorder: RecorderStatus | null;
  last_error: string | null;
  hotkeys: HotkeyState[];
  settings_status: 'missing' | 'loaded' | 'repaired' | 'reset';
  settings_issues: string[];
}

export interface StorageStatus {
  folder: string;
  cap_bytes: number;
  over_cap: 'trash' | 'delete';
  usage: { clips: number; bytes: number; starred: number; starred_bytes: number };
  free_bytes: number | null;
  low_disk: boolean;
}

export interface ClipResult {
  path: string;
  duration_ms: number;
  bytes: number;
  frames: number;
  save_ms: number;
  bookmarks: number;
}

export interface ThumbsReadyEvent {
  event: 'thumbs_ready';
  key: string;
  poster: number | null;
  strip: number | null;
  failed: boolean;
}

export type KlipEvent =
  | { event: 'ready'; version: string; settings: string; issues: string[]; port: number | null }
  | { event: 'game_started'; game: Game }
  | { event: 'game_stopped'; game: Game }
  | {
      event: 'recorder_started';
      game: string | null;
      encoder: string | null;
      width: number | null;
      height: number | null;
      fps: number;
      buffer_ms: number;
      clip_dir: string;
    }
  | { event: 'recorder_stopped'; reason: string }
  | { event: 'recorder_failed'; game: string | null; error: string; retry_secs: number }
  | { event: 'clip_saved'; game: string | null; window: number | null; clip: ClipResult }
  | { event: 'recording_started'; path: string }
  | { event: 'recording_stopped'; summary: unknown }
  | { event: 'bookmark'; result: { recording_ms: number | null } }
  | { event: 'action_failed'; action: string; code: string; message: string }
  | { event: 'quick_menu' }
  | { event: 'mic_level'; peak_db: number; rms_db: number }
  | { event: 'hotkeys'; bound: HotkeyState[] }
  | { event: 'settings_changed'; settings: unknown }
  | { event: 'storage_cleaned'; report: { removed: number; bytes: number } }
  | { event: 'recovered'; path: string; result: unknown }
  | { event: 'warning'; message: string }
  | { event: 'library_changed'; reason: string }
  | ThumbsReadyEvent;
