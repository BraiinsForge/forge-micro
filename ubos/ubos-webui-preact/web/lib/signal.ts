import { useEffect, useState } from 'preact/hooks';

// A minimal reactive value shared across components: mutate `state`,
// then call `notify()` to re-render every component that read it via
// the `useValue` hook. This stands in for a context/state library.
export interface Signal<T> {
  state: T;
  notify: () => void;
  useValue: () => T;
}

export function createSignal<T>(state: T): Signal<T> {
  const listeners = new Set<() => void>();
  function notify() {
    for (const listener of listeners) listener();
  }
  function subscribe(listener: () => void) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  }
  function useValue() {
    const [, setTick] = useState(0);
    useEffect(() => subscribe(() => setTick((t) => t + 1)), []);
    return state;
  }
  return { state, notify, useValue };
}
