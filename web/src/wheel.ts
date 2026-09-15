// Browser wheel deltas are CSS pixels/lines/pages, not Win32 WHEEL_DELTA.
// Map a conventional 100 px / 3 line step to one 120-unit Windows notch.
// Preserve fractional high-resolution trackpad motion across events.
export class WheelDelta {
  #x = 0;
  #y = 0;
  convert(deltaX: number, deltaY: number, mode: number): { x: number; y: number } {
    const scale = mode === 1 ? 40 : mode === 2 ? 120 : 1.2;
    const stepX = Number.isFinite(deltaX) ? deltaX * scale : 0;
    const stepY = Number.isFinite(deltaY) ? -deltaY * scale : 0;
    // The retained remainder belongs to the direction that produced it. Carrying
    // it into a reversal subtracts from the new direction and delays the first
    // unit of it, which reads as the scroll sticking before it turns around.
    if (stepX !== 0 && this.#x !== 0 && Math.sign(stepX) !== Math.sign(this.#x)) this.#x = 0;
    if (stepY !== 0 && this.#y !== 0 && Math.sign(stepY) !== Math.sign(this.#y)) this.#y = 0;
    this.#x += stepX;
    this.#y += stepY;
    const x = Math.max(-32768, Math.min(32767, Math.trunc(this.#x)));
    const y = Math.max(-32768, Math.min(32767, Math.trunc(this.#y)));
    // Retain sub-unit motion only; absurd deltas must not create future scrolls.
    this.#x -= Math.trunc(this.#x);
    this.#y -= Math.trunc(this.#y);
    return { x, y };
  }
}
