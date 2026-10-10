export interface RequestSchedulerOptions<Query, Result> {
  load: (query: Query) => Promise<Result>;
  onStart: (query: Query) => void;
  onSuccess: (result: Result, query: Query) => void;
  onError: (error: unknown, query: Query) => void;
  onSettled: (query: Query) => void;
  queryDelayMs?: number;
  refreshIntervalMs?: number;
}

export interface RequestScheduler<Query> {
  setQuery(query: Query): void;
  refresh(): void;
  dispose(): void;
}

const DEFAULT_QUERY_DELAY_MS = 200;
const DEFAULT_REFRESH_INTERVAL_MS = 1000;

interface PendingRequest<Query> {
  query: Query;
  revision: number;
  dueAt: number;
}

/** Debounces new queries while serializing and coalescing refreshes of the same query. */
export function createRequestScheduler<Query, Result>(
  options: RequestSchedulerOptions<Query, Result>,
): RequestScheduler<Query> {
  const queryDelayMs = options.queryDelayMs ?? DEFAULT_QUERY_DELAY_MS;
  const refreshIntervalMs = options.refreshIntervalMs ?? DEFAULT_REFRESH_INTERVAL_MS;
  if (!Number.isFinite(queryDelayMs) || queryDelayMs < 0
    || !Number.isFinite(refreshIntervalMs) || refreshIntervalMs < 0) {
    throw new RangeError('Request scheduling delays must be finite, non-negative numbers');
  }

  let current: { query: Query; revision: number } | null = null;
  let pending: PendingRequest<Query> | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let revision = 0;
  let active = false;
  let disposed = false;
  let lastStartedAt = Number.NEGATIVE_INFINITY;

  function isCurrent(request: PendingRequest<Query>): boolean {
    return !disposed && current?.revision === request.revision;
  }

  function schedule(): void {
    if (disposed || active || pending === null || timer !== undefined) return;
    timer = setTimeout(start, Math.max(0, pending.dueAt - Date.now()));
  }

  function start(): void {
    timer = undefined;
    if (disposed || active || pending === null) return;
    const request = pending;
    pending = null;
    active = true;
    lastStartedAt = Date.now();
    void execute(request);
  }

  async function execute(request: PendingRequest<Query>): Promise<void> {
    try {
      options.onStart(request.query);
      if (!isCurrent(request)) return;
      const result = await options.load(request.query);
      if (isCurrent(request)) options.onSuccess(result, request.query);
    } catch (error: unknown) {
      if (isCurrent(request)) options.onError(error, request.query);
    } finally {
      active = false;
      try {
        if (isCurrent(request)) options.onSettled(request.query);
      } finally {
        schedule();
      }
    }
  }

  return {
    setQuery(query: Query): void {
      if (disposed) return;
      current = { query, revision: ++revision };
      pending = { ...current, dueAt: Date.now() + queryDelayMs };
      clearTimeout(timer);
      timer = undefined;
      schedule();
    },

    refresh(): void {
      if (disposed || current === null) return;
      // A refresh never postpones an existing query deadline or invalidates its response.
      pending ??= { ...current, dueAt: Math.max(Date.now(), lastStartedAt + refreshIntervalMs) };
      schedule();
    },

    dispose(): void {
      disposed = true;
      current = null;
      pending = null;
      clearTimeout(timer);
      timer = undefined;
    },
  };
}
