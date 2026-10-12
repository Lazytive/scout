import { describe, expect, it } from 'vitest';

import { customChrome, dragsWindow } from './window-chrome';

/** A stand-in for an element: `closest` finds a match when one of `inside` is in the selector list. */
const at = (...inside: string[]) => ({
  closest: (selector: string) =>
    inside.some((s) =>
      selector
        .split(',')
        .map((x) => x.trim())
        .includes(s),
    )
      ? {}
      : null,
});

describe('own title bar (M11.3)', () => {
  it('moves the window from empty parts, never from a control or a pop-up', () => {
    expect(dragsWindow(at())).toBe(true);
    for (const c of ['button', 'input', 'a', 'select', '[role="dialog"]', '[role="menu"]', '[data-no-drag]']) {
      expect(dragsWindow(at(c))).toBe(false);
    }
    expect(dragsWindow(null)).toBe(false);
  });

  it('is off outside the app unless asked for', () => {
    expect(customChrome).toBe(false);
  });
});
