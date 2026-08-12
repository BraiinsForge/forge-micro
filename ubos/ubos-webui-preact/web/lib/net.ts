import { createSignal } from './signal';
import type { NetChange } from './types';

const sig = createSignal<{ changed: NetChange | null }>({ changed: null });
export const net = sig.state;
export const useNet = sig.useValue;

export function setNetChanged(value: NetChange | null): void {
  net.changed = value;
  sig.notify();
}
