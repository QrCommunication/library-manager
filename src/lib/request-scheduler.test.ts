import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRequestScheduler } from './request-scheduler';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((fulfilled, rejected) => { resolve = fulfilled; reject = rejected; });
  return { promise, resolve, reject };
}

const disposers: Array<() => void> = [];

function fixture(load: (query: string) => Promise<string>) {
  const onStart = vi.fn();
  const onSuccess = vi.fn();
  const onError = vi.fn();
  const onSettled = vi.fn();
  const scheduler = createRequestScheduler<string, string>({ load, onStart, onSuccess, onError, onSettled });
  disposers.push(() => scheduler.dispose());
  return { scheduler, onStart, onSuccess, onError, onSettled };
}

beforeEach(() => { vi.useFakeTimers(); vi.setSystemTime(0); });
afterEach(() => {
  for (const dispose of disposers.splice(0)) dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe('library request scheduling', () => {
  it('publishes the initial page by 500 ms during continuous 50 ms refresh events', async () => {
    const load = vi.fn((query: string) => new Promise<string>((resolve) => {
      setTimeout(() => resolve(`page:${query}`), 300);
    }));
    const { scheduler, onSuccess, onSettled } = fixture(load);
    scheduler.setQuery('all');
    const events = setInterval(() => scheduler.refresh(), 50);

    await vi.advanceTimersByTimeAsync(499);
    expect(load).toHaveBeenCalledTimes(1);
    expect(onSuccess).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(onSuccess).toHaveBeenCalledExactlyOnceWith('page:all', 'all');
    expect(onSettled).toHaveBeenCalledExactlyOnceWith('all');

    await vi.advanceTimersByTimeAsync(1_500);
    clearInterval(events);
    expect(onSuccess.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(load.mock.calls.length).toBeLessThanOrEqual(3);
  });

  it('keeps one request active and collapses an event backlog into one follow-up', async () => {
    const pending: Array<ReturnType<typeof deferred<string>>> = [];
    const load = vi.fn((_query: string) => {
      const request = deferred<string>();
      pending.push(request);
      return request.promise;
    });
    const { scheduler, onSuccess } = fixture(load);
    scheduler.setQuery('all');
    await vi.advanceTimersByTimeAsync(200);
    for (let event = 0; event < 100; event += 1) scheduler.refresh();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(load).toHaveBeenCalledTimes(1);

    pending[0]!.resolve('first');
    await vi.advanceTimersByTimeAsync(1_000);
    expect(onSuccess).toHaveBeenCalledWith('first', 'all');
    expect(load).toHaveBeenCalledTimes(2);
    pending[1]!.resolve('follow-up');
    await vi.advanceTimersByTimeAsync(2_000);
    expect(onSuccess).toHaveBeenCalledWith('follow-up', 'all');
    expect(load).toHaveBeenCalledTimes(2);
  });

  it('discards an old response and debounces only the newest search while refreshes continue', async () => {
    const old = deferred<string>();
    const load = vi.fn((query: string) => query === 'A' ? old.promise : Promise.resolve(`page:${query}`));
    const { scheduler, onSuccess, onSettled } = fixture(load);
    scheduler.setQuery('A');
    await vi.advanceTimersByTimeAsync(200);
    await vi.advanceTimersByTimeAsync(50);
    scheduler.setQuery('B');
    await vi.advanceTimersByTimeAsync(100);
    scheduler.setQuery('C');
    old.resolve('stale:A');
    const events = setInterval(() => scheduler.refresh(), 50);
    await vi.advanceTimersByTimeAsync(199);
    expect(load).toHaveBeenCalledTimes(1);
    expect(onSuccess).not.toHaveBeenCalled();
    expect(onSettled).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    clearInterval(events);
    expect(load.mock.calls.map(([query]) => query)).toEqual(['A', 'C']);
    expect(onSuccess).toHaveBeenCalledExactlyOnceWith('page:C', 'C');
    expect(onSettled).toHaveBeenCalledExactlyOnceWith('C');
  });

  it('does not publish an obsolete error or settle the newer query', async () => {
    const old = deferred<string>();
    const load = vi.fn((query: string) => query === 'old' ? old.promise : Promise.resolve('current page'));
    const { scheduler, onError, onSuccess, onSettled } = fixture(load);
    scheduler.setQuery('old');
    await vi.advanceTimersByTimeAsync(200);
    scheduler.setQuery('current');
    old.reject(new Error('obsolete failure'));
    await vi.advanceTimersByTimeAsync(0);
    expect(onError).not.toHaveBeenCalled();
    expect(onSettled).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(200);
    expect(onSuccess).toHaveBeenCalledExactlyOnceWith('current page', 'current');
    expect(onSettled).toHaveBeenCalledExactlyOnceWith('current');
  });

  it('cancels a pending query and refuses new work after disposal', async () => {
    const load = vi.fn((_query: string) => Promise.resolve('page'));
    const { scheduler, onStart, onSuccess, onError, onSettled } = fixture(load);
    scheduler.setQuery('pending');
    scheduler.refresh();
    scheduler.dispose();
    scheduler.setQuery('after disposal');
    scheduler.refresh();
    await vi.advanceTimersByTimeAsync(5_000);
    expect(load).not.toHaveBeenCalled();
    for (const callback of [onStart, onSuccess, onError, onSettled]) expect(callback).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each(['success', 'failure'] as const)('ignores in-flight %s and pending refreshes after disposal', async (outcome) => {
    const pending = deferred<string>();
    const load = vi.fn((_query: string) => pending.promise);
    const { scheduler, onSuccess, onError, onSettled } = fixture(load);
    scheduler.setQuery('all');
    await vi.advanceTimersByTimeAsync(200);
    scheduler.refresh();
    scheduler.dispose();
    if (outcome === 'success') pending.resolve('late page');
    else pending.reject(new Error('late error'));
    await vi.advanceTimersByTimeAsync(5_000);
    expect(load).toHaveBeenCalledTimes(1);
    for (const callback of [onSuccess, onError, onSettled]) expect(callback).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('reports a current failure, settles loading and recovers on a subsequent refresh', async () => {
    const error = new Error('current failure');
    const load = vi.fn<(query: string) => Promise<string>>()
      .mockRejectedValueOnce(error)
      .mockResolvedValueOnce('recovered page');
    const { scheduler, onError, onSuccess, onSettled } = fixture(load);
    scheduler.setQuery('all');
    await vi.advanceTimersByTimeAsync(200);
    expect(onError).toHaveBeenCalledExactlyOnceWith(error, 'all');
    expect(onSettled).toHaveBeenCalledExactlyOnceWith('all');
    expect(onSuccess).not.toHaveBeenCalled();
    scheduler.refresh();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(load).toHaveBeenCalledTimes(2);
    expect(onSuccess).toHaveBeenCalledExactlyOnceWith('recovered page', 'all');
    expect(onSettled).toHaveBeenCalledTimes(2);
  });
});
