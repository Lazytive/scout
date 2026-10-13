// The "restart now or later" warning (the user, 2026-10-13): some settings reach the
// recorder only when it starts; restarting it now clears the replay buffer.

/** "Quality", "Quality and Audio", "Quality, Audio and Clip folders". */
export function joinReasons(reasons: string[]): string {
  if (reasons.length <= 1) return reasons[0] ?? '';
  return `${reasons.slice(0, -1).join(', ')} and ${reasons[reasons.length - 1]}`;
}

/** The warning's body: what changed, what a restart costs now, what Later means. */
export function restartMessage(reasons: string[], buffered: string | null, recording: boolean): string {
  const what = joinReasons(reasons);
  const verb = reasons.length === 1 ? 'changes' : 'change';
  const cost = buffered ? `clears the replay buffer (${buffered} in it now)` : 'clears the replay buffer';
  const rec = recording ? '; your recording goes on in a new file' : '';
  return `${what} ${verb} only when the recorder restarts. Restarting now ${cost}${rec}. Later: the new settings apply the next time the recorder starts.`;
}
