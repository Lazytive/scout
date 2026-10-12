import { describe, expect, it } from 'vitest';

import { latestOnly } from './latest';

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('latestOnly', () => {
  it('ignores a reply that arrives after a newer request (IMPROVEMENTS B7)', async () => {
    const take = latestOnly();
    const got: string[] = [];
    const first = deferred<string>();
    const second = deferred<string>();
    take(first.promise, (v) => got.push(v));
    take(second.promise, (v) => got.push(v));
    second.resolve('new');
    await tick();
    first.resolve('old');
    await tick();
    expect(got).toEqual(['new']);
  });

  it('reports only the newest failure and never leaves a rejection unhandled', async () => {
    const take = latestOnly();
    const errors: unknown[] = [];
    const first = deferred<number>();
    const second = deferred<number>();
    take(
      first.promise,
      () => undefined,
      (e) => errors.push(e),
    );
    take(
      second.promise,
      () => undefined,
      (e) => errors.push(e),
    );
    first.reject(new Error('stale'));
    second.reject(new Error('current'));
    await tick();
    expect(errors.map((e) => (e as Error).message)).toEqual(['current']);
    // No `fail`: a rejection is still handled.
    const third = deferred<number>();
    take(third.promise, () => undefined);
    third.reject(new Error('ignored'));
    await tick();
  });
});
