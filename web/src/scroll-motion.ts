export interface ScrollRegion { x: number; y: number; width: number; height: number }
export interface ScrollMotion { region: ScrollRegion; dy: number; confidence: number }

// Small grayscale thumbnails only. Never move the entire desktop: infer the
// changed rectangle and require a strong vertical translation match inside it.
export function detectScroll(before: Uint8Array, after: Uint8Array, width: number, height: number): ScrollMotion | null {
  if (before.length !== width * height || after.length !== before.length) return null;
  let left = width, right = -1, top = height, bottom = -1, changed = 0;
  for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
    const i = y * width + x;
    if (Math.abs(before[i]! - after[i]!) > 22) {
      changed++; left = Math.min(left, x); right = Math.max(right, x); top = Math.min(top, y); bottom = Math.max(bottom, y);
    }
  }
  if (changed < width * height * .025 || right - left < 24 || bottom - top < 24) return null;
  const error = (dy: number): number => {
    let sum = 0, count = 0;
    for (let y = top + 18; y <= bottom - 18; y += 2) for (let x = left; x <= right; x += 2) {
      sum += Math.abs(after[y * width + x]! - before[(y - dy) * width + x]!); count++;
    }
    return count ? sum / count : Infinity;
  };
  const still = error(0);
  let best = still, dy = 0, second = Infinity;
  for (let shift = -16; shift <= 16; shift++) {
    if (!shift) continue;
    const value = error(shift);
    if (value < best) { second = best; best = value; dy = shift; }
    else second = Math.min(second, value);
  }
  if (!dy || still < 6 || best > 12 || best > still * .35 || second - best < 1) return null;
  // Any stationary, textured band inside the proposed region means it may
  // contain a sticky header/overlay. Abstain instead of dragging that UI.
  for (let y = top + 18; y < bottom - 18; y += 8) {
    let stationary = 0, moving = 0, texture = 0;
    for (let x = left + 1; x <= right; x++) {
      const i = y * width + x;
      stationary += Math.abs(after[i]! - before[i]!);
      moving += Math.abs(after[i]! - before[i - dy * width]!);
      texture += Math.abs(before[i]! - before[i - 1]!);
    }
    if (texture > (right - left) * 8 && stationary < moving * .4) return null;
  }
  return { region: { x: left, y: top, width: right - left + 1, height: bottom - top + 1 }, dy, confidence: 1 - best / still };
}

export function predictionOffset(velocity: number, frameAgeMs: number, wheelAgeMs: number, direction: number): number {
  if (wheelAgeMs > 120 || frameAgeMs > 140 || Math.sign(velocity) !== direction) return 0;
  // Leave normal 60 Hz video untouched. Predict only when a frame is late,
  // ramping from zero rather than repeatedly shifting every decoded frame.
  return Math.sign(velocity) * Math.min(6, Math.abs(velocity) * Math.min(60, Math.max(0, frameAgeMs - 24)));
}
