export type PointerPosition = { x: number; y: number };

// A fresh immediately-sendable move supersedes any old buffered move too.
export class LatestPointer {
  #pending: PointerPosition | null = null;
  offer(move: PointerPosition, writable: boolean): PointerPosition | null {
    this.#pending = writable ? null : move;
    return writable ? move : null;
  }
  take(): PointerPosition | null {
    const move = this.#pending;
    this.#pending = null;
    return move;
  }
}

export function requestInteractivePlayback(receiver: {
  track?: { kind: string }; jitterBufferTarget?: number | null; playoutDelayHint?: number | null;
}): void {
  if (receiver.track?.kind !== "video") return;
  // Hints, not a promise of zero network/decode/display latency.
  try { receiver.jitterBufferTarget = 0; } catch { /* browser chooses its safe minimum */ }
  try { receiver.playoutDelayHint = 0; } catch { /* older browser */ }
}
