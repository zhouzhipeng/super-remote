import { detectScroll, predictionOffset, type ScrollMotion } from "./scroll-motion.ts";

const WIDTH = 160, HEIGHT = 96;
export class ScrollPreview {
  #video: HTMLVideoElement;
  #overlay = document.createElement("canvas");
  #thumb = document.createElement("canvas");
  #frames = [document.createElement("canvas"), document.createElement("canvas")];
  #previous: Uint8Array | null = null;
  #motion: ScrollMotion | null = null;
  #frameAt = 0;
  #sampleAt = 0;
  #wheelAt = -Infinity;
  #direction = 0;
  #velocity = 0;
  #confirmed = 0;
  #frameCallback = 0;
  #animation = 0;
  #enabled = true;
  #destroyed = false;
  #cooldownUntil = 0;
  #point = { x: -1, y: -1 };
  #resize: ResizeObserver;

  constructor(video: HTMLVideoElement) {
    this.#video = video;
    this.#overlay.className = "scroll-preview";
    this.#overlay.setAttribute("aria-hidden", "true");
    this.#overlay.hidden = true;
    video.parentElement?.append(this.#overlay);
    this.#thumb.width = WIDTH; this.#thumb.height = HEIGHT;
    video.addEventListener("wheel", this.#wheel, { passive: true });
    video.addEventListener("pointerdown", this.#reset);
    window.addEventListener("keydown", this.#reset, true);
    window.addEventListener("blur", this.#reset);
    document.addEventListener("visibilitychange", this.#reset);
    this.#resize = new ResizeObserver(this.#reset); this.#resize.observe(video);
    if (typeof video.requestVideoFrameCallback === "function") this.#frameCallback = video.requestVideoFrameCallback(this.#frame);
  }
  set enabled(value: boolean) { this.#enabled = value; this.#reset(); }
  destroy(): void {
    this.#destroyed = true; this.#reset();
    this.#video.cancelVideoFrameCallback?.(this.#frameCallback);
    this.#video.removeEventListener("wheel", this.#wheel);
    this.#video.removeEventListener("pointerdown", this.#reset);
    window.removeEventListener("keydown", this.#reset, true);
    window.removeEventListener("blur", this.#reset);
    document.removeEventListener("visibilitychange", this.#reset);
    this.#resize.disconnect(); this.#overlay.remove();
    for (const frame of this.#frames) { frame.width = 0; frame.height = 0; }
    delete this.#video.dataset.scrollPreview;
  }
  #reset = (): void => {
    this.#motion = null; this.#previous = null; this.#confirmed = 0; this.#velocity = 0;
    this.#wheelAt = -Infinity; this.#overlay.hidden = true;
    cancelAnimationFrame(this.#animation); this.#animation = 0;
    this.#video.dataset.scrollPreview = this.#enabled ? "learning" : "off";
  };
  #wheel = (event: WheelEvent): void => {
    if (performance.now() < this.#cooldownUntil) return;
    if (!this.#enabled || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey
      || !event.deltaY || Math.abs(event.deltaX) > Math.abs(event.deltaY) * .2) { this.#reset(); return; }
    const rect = this.#displayRect();
    this.#point = { x: (event.clientX - rect.x) / rect.width * WIDTH, y: (event.clientY - rect.y) / rect.height * HEIGHT };
    const direction = -Math.sign(event.deltaY);
    if (direction !== this.#direction || performance.now() - this.#wheelAt > 250) this.#reset();
    this.#direction = direction; this.#wheelAt = performance.now();
    if (!this.#animation) this.#animation = requestAnimationFrame(this.#draw);
  };
  #displayRect(): DOMRect {
    const box = this.#video.getBoundingClientRect();
    const scale = Math.min(box.width / this.#video.videoWidth, box.height / this.#video.videoHeight);
    const width = this.#video.videoWidth * scale, height = this.#video.videoHeight * scale;
    return new DOMRect(box.x + (box.width - width) / 2, box.y + (box.height - height) / 2, width, height);
  }
  #frame = (now: number): void => {
    if (this.#destroyed) return;
    this.#frameCallback = this.#video.requestVideoFrameCallback(this.#frame);
    if (!this.#enabled || document.hidden || now - this.#wheelAt > 240) { this.#overlay.hidden = true; return; }
    // Even frames skipped by the 30 Hz motion estimator must replace the
    // cached pixels. Otherwise the overlay can cover a newer authoritative
    // frame with an older one and itself introduce visible stepping.
    this.#overlay.hidden = true;
    if (now - this.#sampleAt < 30) {
      if (this.#motion && this.#confirmed >= 2) {
        try { this.#cacheFrame(now); } catch { this.#reset(); }
      }
      return;
    }
    const started = performance.now();
    try {
      const ctx = this.#thumb.getContext("2d", { willReadFrequently: true })!;
      ctx.drawImage(this.#video, 0, 0, WIDTH, HEIGHT);
      const rgba = ctx.getImageData(0, 0, WIDTH, HEIGHT).data;
      const gray = new Uint8Array(WIDTH * HEIGHT);
      for (let i = 0; i < gray.length; i++) gray[i] = (rgba[i * 4]! + rgba[i * 4 + 1]! * 2 + rgba[i * 4 + 2]!) / 4;
      const motion = this.#previous ? detectScroll(this.#previous, gray, WIDTH, HEIGHT) : null;
      if (motion && Math.sign(motion.dy) === this.#direction) {
        const r = motion.region, p = this.#point;
        if (p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height) {
          const previous = this.#motion?.region;
          const stable = previous && Math.abs(previous.x - r.x) <= 3 && Math.abs(previous.y - r.y) <= 3
            && Math.abs(previous.width - r.width) <= 6 && Math.abs(previous.height - r.height) <= 6;
          this.#motion = motion; this.#confirmed = stable ? this.#confirmed + 1 : 1;
          this.#velocity = motion.dy / Math.max(16, now - this.#sampleAt);
        } else { this.#motion = null; this.#confirmed = 0; }
      } else { this.#motion = null; this.#confirmed = 0; }
      this.#previous = gray; this.#sampleAt = now;
      if (this.#motion && this.#confirmed >= 2) {
        this.#cacheFrame(now);
      } else this.#overlay.hidden = true;
      // Readbacks must never become the source of desktop stutter.
      if (performance.now() - started > 8) {
        this.#cooldownUntil = performance.now() + 2000;
        this.#reset(); this.#video.dataset.scrollPreview = "limited";
      }
    } catch { this.#reset(); }
  };
  #cacheFrame(now: number): void {
    // Preserve physical pixels: a downscaled overlay visibly blurs HiDPI text.
    const width = this.#video.videoWidth, height = this.#video.videoHeight;
    const cached = this.#frames.pop()!; this.#frames.unshift(cached);
    if (cached.width !== width || cached.height !== height) { cached.width = width; cached.height = height; }
    cached.getContext("2d")!.drawImage(this.#video, 0, 0, width, height);
    this.#frameAt = now;
  }
  #draw = (now: number): void => {
    this.#animation = 0;
    const offset = predictionOffset(this.#velocity, now - this.#frameAt, now - this.#wheelAt, this.#direction);
    if (this.#enabled && this.#motion && this.#confirmed >= 2 && offset) {
      const display = this.#displayRect(), parent = this.#video.parentElement!.getBoundingClientRect();
      const frame = this.#frames[0]!, r = this.#motion.region;
      const sx = frame.width / WIDTH, sy = frame.height / HEIGHT;
      if (this.#overlay.width !== frame.width || this.#overlay.height !== frame.height) {
        this.#overlay.width = frame.width; this.#overlay.height = frame.height;
      }
      Object.assign(this.#overlay.style, { left: `${display.x - parent.x}px`, top: `${display.y - parent.y}px`, width: `${display.width}px`, height: `${display.height}px` });
      const ctx = this.#overlay.getContext("2d")!;
      ctx.clearRect(0, 0, frame.width, frame.height);
      ctx.save(); ctx.beginPath(); ctx.rect(r.x * sx, r.y * sy, r.width * sx, r.height * sy); ctx.clip();
      // Only already-seen pixels. Unseen exposed strips stay transparent and
      // show the authoritative video; never synthesize text or future content.
      ctx.drawImage(frame, r.x * sx, r.y * sy, r.width * sx, r.height * sy,
        r.x * sx, (r.y + offset) * sy, r.width * sx, r.height * sy);
      ctx.restore(); this.#overlay.hidden = false;
      this.#video.dataset.scrollPreview = "predicting";
    } else this.#overlay.hidden = true;
    if (now - this.#wheelAt <= 160 && !this.#destroyed) this.#animation = requestAnimationFrame(this.#draw);
  };
}
