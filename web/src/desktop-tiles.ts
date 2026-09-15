const MAX_BYTES = 128 * 1024 * 1024;
export type Tile = { x: number; y: number; width: number; height: number; png: Uint8Array<ArrayBuffer> };

export function parseTiles(data: Uint8Array<ArrayBuffer>, count: number, width: number, height: number): Tile[] {
  if (!Number.isInteger(count) || count < 1 || count > 2304) throw new Error("Invalid tile count");
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const tiles: Tile[] = [];
  let offset = 0;
  for (let i = 0; i < count; i++) {
    if (offset + 12 > data.length) throw new Error("Truncated tile header");
    const x = view.getUint16(offset, true), y = view.getUint16(offset + 2, true);
    const w = view.getUint16(offset + 4, true), h = view.getUint16(offset + 6, true);
    const size = view.getUint32(offset + 8, true);
    offset += 12;
    if (!w || !h || w > 128 || h > 128 || x + w > width || y + h > height
      || size < 8 || size > 128 * 1024 || offset + size > data.length) throw new Error("Invalid tile bounds");
    tiles.push({ x, y, width: w, height: h, png: data.subarray(offset, offset + size) });
    offset += size;
  }
  if (offset !== data.length) throw new Error("Trailing desktop data");
  return tiles;
}

type CopyRect = { x: number; y: number; width: number; height: number; source_y: number };
export function parseCopies(value: unknown, width: number, height: number): CopyRect[] {
  if (!Array.isArray(value) || value.length > 2304) throw new Error("Invalid copy list");
  for (const copy of value) {
    if (!copy || ![copy.x,copy.y,copy.width,copy.height,copy.source_y].every(Number.isInteger)
      || copy.x < 0 || copy.y < 0 || copy.source_y < 0 || copy.width < 1 || copy.width > 128
      || copy.height < 1 || copy.height > 128 || copy.x + copy.width > width
      || copy.y + copy.height > height || copy.source_y + copy.height > height) throw new Error("Invalid copy bounds");
  }
  return value;
}
type Update = { id: number; width: number; height: number; tiles: number; copies: CopyRect[];
  data: Uint8Array<ArrayBuffer>; offset: number; receivedAt: number };

export class DesktopTiles {
  #canvas = document.createElement("canvas");
  #back = document.createElement("canvas");
  #baseline = document.createElement("canvas");
  #pending: Update | null = null;
  #closed = false;
  #committing = false;
  #queue: Update[] = [];
  #receivedId = 0;
  #queuedBytes = 0;
  #lastId = 0;
  #bytes = 0;
  #videoReadySent = false;
  #qualityTransition: Animation | null = null;
  #resize: ResizeObserver;
  private video: HTMLVideoElement;
  private channel: RTCDataChannel;
  private ready: () => void;
  constructor(video: HTMLVideoElement, channel: RTCDataChannel, ready: () => void) {
    this.video = video; this.channel = channel; this.ready = ready;
    this.#canvas.className = "desktop-tiles";
    this.#canvas.hidden = true;
    video.parentElement!.append(this.#canvas);
    this.#resize = new ResizeObserver(() => {
      Object.assign(this.#canvas.style, { left: `${video.offsetLeft}px`, top: `${video.offsetTop}px`,
        width: `${video.offsetWidth}px`, height: `${video.offsetHeight}px` });
    });
    this.#resize.observe(video);
    video.addEventListener("remote-input", this.#hide);
    video.addEventListener("playing", this.#videoReady);
    channel.binaryType = "arraybuffer";
    channel.addEventListener("open", this.#open);
    channel.addEventListener("message", this.#message);
    channel.addEventListener("close", this.destroy);
  }
  #hide = (): void => {
    this.#qualityTransition?.cancel(); this.#qualityTransition = null;
    this.#canvas.hidden = true;
    this.video.dataset.displayTransport = "hybrid-video";
  };
  #videoReady = (): void => {
    if (!this.#videoReadySent && this.channel.readyState === "open"
      && this.video.readyState >= 2 && this.video.videoWidth > 0) {
      this.#videoReadySent = true;
      this.channel.send("video-ready");
    }
  };
  #open = (): void => { this.channel.send("start"); this.#videoReady(); };
  #message = (event: MessageEvent): void => {
    if (this.#closed) return;
    try {
      if (typeof event.data !== "string") {
        const pending = this.#pending;
        if (!pending || !(event.data instanceof ArrayBuffer) || pending.offset + event.data.byteLength > pending.data.length) throw new Error("Unexpected desktop bytes");
        pending.data.set(new Uint8Array(event.data), pending.offset);
        pending.offset += event.data.byteLength;
        return;
      }
      const message = JSON.parse(event.data);
      if (message.type === "invalidate") {
        this.#hide();
      } else if (message.type === "show") {
        if (performance.now() >= Number(this.video.dataset.wheelActiveUntil || 0)
          && message.id === this.#lastId && typeof message.input === "string"
          && /^\d+$/.test(message.input)
          && BigInt(message.input) >= BigInt(this.video.dataset.latestInput || "0")) {
          const cols = Math.ceil(this.#baseline.width / 128), rows = Math.ceil(this.#baseline.height / 128);
          const hidden: number[] = message.hidden ?? [];
          if (!Array.isArray(hidden) || hidden.length > cols * rows || hidden.some(index =>
            !Number.isInteger(index) || index < 0 || index >= cols * rows)) throw new Error("Invalid refinement mask");
          const resized = this.#canvas.width !== this.#baseline.width || this.#canvas.height !== this.#baseline.height;
          const reveal = this.#canvas.hidden || resized;
          // Resize only when validated pixels can be drawn in this same task.
          // Changing canvas dimensions during decode clears the visible bitmap.
          if (resized) {
            this.#canvas.width = this.#baseline.width; this.#canvas.height = this.#baseline.height;
          }
          const context = this.#canvas.getContext("2d")!;
          context.clearRect(0, 0, this.#canvas.width, this.#canvas.height);
          context.drawImage(this.#baseline, 0, 0);
          for (const index of hidden) context.clearRect(index % cols * 128, Math.floor(index / cols) * 128, 128, 128);
          this.#canvas.hidden = hidden.length === cols * rows;
          if (reveal && !this.#canvas.hidden) {
            this.#qualityTransition?.cancel();
            this.#qualityTransition = this.#canvas.animate([{ opacity: 0 }, { opacity: 1 }],
              { duration: 100, easing: "ease-out" });
          }
          this.video.dataset.displayTransport = this.#canvas.hidden ? "hybrid-video" : "lossless-tiles";
        }
      } else if (message.type === "cancel") {
        if (!this.#pending || this.#pending.id !== message.id) throw new Error("Unexpected cancellation");
        this.#queuedBytes -= this.#pending.data.length;
        this.#pending = null;
        this.#receivedId = message.id;
      } else if (message.type === "begin") {
        if (this.#pending || this.#queue.length >= 8 || !Number.isInteger(message.id) || message.id !== this.#receivedId + 1
          || !Number.isInteger(message.width) || message.width < 1 || message.width > 8192
          || !Number.isInteger(message.height) || message.height < 1 || message.height > 4320
          || !Number.isInteger(message.bytes) || message.bytes < 0 || this.#queuedBytes + message.bytes > MAX_BYTES
          || !Number.isInteger(message.tiles) || message.tiles < 0 || message.tiles > 2304) throw new Error("Invalid desktop update");
        const copies = parseCopies(message.copies ?? [], message.width, message.height);
        if (!message.tiles && !copies.length) throw new Error("Empty desktop update");
        this.#queuedBytes += message.bytes;
        this.#pending = { ...message, copies, data: new Uint8Array(message.bytes), offset: 0, receivedAt: performance.now() };
      } else if (message.type === "end") {
        const update = this.#pending;
        if (!update || update.id !== message.id || update.offset !== update.data.length) throw new Error("Incomplete desktop update");
        this.#pending = null;
        this.#receivedId = update.id;
        this.#queue.push(update);
        void this.#drain().catch(() => this.destroy());
      } else throw new Error("Unknown desktop message");
    } catch { this.destroy(); }
  };
  async #drain(): Promise<void> {
    if (this.#committing) return;
    this.#committing = true;
    try {
      while (!this.#closed && this.#queue.length) {
        const update = this.#queue.shift()!;
        await this.#commit(update);
        this.#queuedBytes -= update.data.length;
      }
    } finally { this.#committing = false; }
  }
  async #commit(update: Update): Promise<void> {
    const decodeStarted = performance.now();
    const tiles = update.tiles === 0 && update.data.length === 0 ? []
      : parseTiles(update.data, update.tiles, update.width, update.height);
    if (!this.#lastId || this.#back.width !== update.width || this.#back.height !== update.height) {
      // A resize must establish a complete new baseline before any delta.
      const expected = Math.ceil(update.width / 128) * Math.ceil(update.height / 128);
      const positions = new Set(tiles.map(tile => `${tile.x},${tile.y}`));
      if (update.copies.length || tiles.length !== expected || positions.size !== expected || tiles.some(tile =>
        tile.x % 128 !== 0 || tile.y % 128 !== 0 || tile.width !== Math.min(128, update.width - tile.x)
        || tile.height !== Math.min(128, update.height - tile.y))) throw new Error("Incomplete baseline");
      this.#back.width = update.width; this.#back.height = update.height;
    }
    const context = this.#back.getContext("2d", { alpha: false })!;
    // Read every scroll-copy from the PREVIOUS committed image, never from a
    // region another copy has already overwritten in the current update.
    for (const copy of update.copies) {
      context.drawImage(this.#baseline, copy.x, copy.source_y, copy.width, copy.height,
        copy.x, copy.y, copy.width, copy.height);
    }
    // Decode in small batches so a full desktop does not monopolize the main
    // thread or allocate hundreds of simultaneous bitmap decoders.
    for (let offset = 0; offset < tiles.length; offset += 8) {
      const batch = tiles.slice(offset, offset + 8);
      const decoded = await Promise.allSettled(batch.map(tile =>
        createImageBitmap(new Blob([tile.png], { type: "image/png" }))));
      try {
        if (this.#closed) return;
        for (let i = 0; i < batch.length; i++) {
          const result = decoded[i]!, tile = batch[i]!;
          if (result.status !== "fulfilled") throw new Error("PNG decoding failed");
          const bitmap = result.value;
          if (bitmap.width !== tile.width || bitmap.height !== tile.height) throw new Error("Unexpected PNG dimensions");
          context.drawImage(bitmap, tile.x, tile.y);
        }
      } finally { for (const result of decoded) if (result.status === "fulfilled") result.value.close(); }
    }
    if (this.#closed) return;
    if (this.#baseline.width !== update.width || this.#baseline.height !== update.height) {
      this.#baseline.width = update.width; this.#baseline.height = update.height;
    }
    this.#baseline.getContext("2d", { alpha: false })!.drawImage(this.#back, 0, 0);
    // Keep the last validated display visible while the new baseline awaits
    // host validation. A commit is not an input event and must not flash video.
    this.video.dataset.desktopWidth = String(update.width);
    this.video.dataset.desktopHeight = String(update.height);
    this.video.dataset.tileCount = String(update.tiles);
    this.video.dataset.tileCopies = String(update.copies.length);
    this.video.dataset.tileDecodeMs = (performance.now() - decodeStarted).toFixed(1);
    this.video.dataset.tileReceiveToCommitMs = (performance.now() - update.receivedAt).toFixed(1);
    this.#bytes += update.data.length;
    this.video.dataset.tileBytes = String(this.#bytes);
    this.video.dataset.tileFrame = String(update.id);
    this.#lastId = update.id;
    this.ready();
    this.channel.send(JSON.stringify({ type: "ack", id: update.id }));
  }
  destroy = (): void => {
    if (this.#closed) return;
    this.#closed = true;
    this.#qualityTransition?.cancel(); this.#qualityTransition = null;
    this.#pending = null;
    this.#queue.length = 0;
    this.channel.removeEventListener("open", this.#open);
    this.channel.removeEventListener("message", this.#message);
    this.channel.removeEventListener("close", this.destroy);
    this.video.removeEventListener("remote-input", this.#hide);
    this.video.removeEventListener("playing", this.#videoReady);
    this.channel.close();
    this.#resize.disconnect();
    this.#canvas.remove();
    this.#canvas.width = this.#back.width = this.#baseline.width = 0;
    this.#canvas.height = this.#back.height = this.#baseline.height = 0;
    for (const key of ["desktopWidth", "desktopHeight", "displayTransport", "tileCount", "tileBytes", "tileFrame", "tileCopies", "tileDecodeMs", "tileReceiveToCommitMs"]) delete this.video.dataset[key];
  };
}
