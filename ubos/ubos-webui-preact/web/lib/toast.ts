// Global toast notifications: a single top-right stack driven by a
// module-level signal, so any form can raise one without prop drilling.
// Mirrors lib/store.ts — mutate the shared state, then notify().

import { createSignal } from './signal';
import type { Toast, ToastKind } from './types';

const SUCCESS_TTL = 5000; // success self-dismisses; errors persist until closed

export const state: { list: Toast[] } = { list: [] };
const sig = createSignal(state);
export const useToasts = sig.useValue;

let nextId = 1;

// `text` is optional detail below the title. `ttl` in ms auto-dismisses;
// a falsy ttl keeps the toast until closed.
function push(kind: ToastKind, title: string, text?: string, ttl?: number): number {
  const id = nextId++;
  state.list = [...state.list, { id, kind, title, text }];
  sig.notify();
  if (ttl) setTimeout(() => dismiss(id), ttl);
  return id;
}

export function dismiss(id: number): void {
  state.list = state.list.filter((t) => t.id !== id);
  sig.notify();
}

export const toast = {
  ok: (title: string, text?: string, ttl: number = SUCCESS_TTL) => push('success', title, text, ttl),
  err: (title: string, text?: string, ttl: number = 0) => push('error', title, text, ttl),
};

// Narrow an unknown throw to its message for a toast detail line.
export const errMsg = (error: unknown): string => (error instanceof Error ? error.message : String(error));
