// Only the newest reply counts: an estimate or encoder list for settings the user has
// already changed again can arrive after the current one and must not overwrite it.

export type Take = <T>(p: Promise<T>, apply: (v: T) => void, fail?: (e: unknown) => void) => void;

/** `const take = latestOnly()`, then `take(call(...), apply, fail)` on every request. */
export function latestOnly(): Take {
  let seq = 0;
  return (p, apply, fail) => {
    seq += 1;
    const mine = seq;
    p.then(
      (v) => {
        if (mine === seq) apply(v);
      },
      (e: unknown) => {
        if (mine === seq) fail?.(e);
      },
    );
  };
}
