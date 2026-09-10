const SHAPES = new Set(["default", "text", "pointer", "wait", "progress", "crosshair", "move", "ns-resize", "ew-resize", "nesw-resize", "nwse-resize", "not-allowed", "help"]);

export function cursorStyle(value: unknown): string | null {
  if (!value || typeof value !== "object") return null;
  const state = value as { visible?: unknown; shape?: unknown; image?: { png?: unknown; x?: unknown; y?: unknown } };
  if (typeof state.visible !== "boolean" || typeof state.shape !== "string" || !SHAPES.has(state.shape)) return null;
  if (!state.visible) return "none";
  const image = state.image;
  if (image && typeof image.png === "string" && image.png.length <= 100_000 && /^[A-Za-z0-9+/]+={0,2}$/.test(image.png)
    && Number.isInteger(image.x) && Number.isInteger(image.y) && Number(image.x) >= 0 && Number(image.x) < 128 && Number(image.y) >= 0 && Number(image.y) < 128) {
    return `url("data:image/png;base64,${image.png}") ${image.x} ${image.y}, ${state.shape}`;
  }
  return state.shape;
}

export class LocalCursor {
  #video: HTMLVideoElement;
  #channel: RTCDataChannel;
  #enabled = false;
  constructor(video: HTMLVideoElement, channel: RTCDataChannel) {
    this.#video = video; this.#channel = channel;
    video.style.cursor = "none";
    channel.addEventListener("message", this.#message);
    channel.addEventListener("close", this.#closed);
  }
  enable(enabled: boolean): void {
    this.#enabled = enabled;
    this.#video.style.cursor = enabled ? "default" : "none";
    this.#video.dataset.cursorMode = enabled ? "local" : "video";
  }
  destroy(): void {
    this.#channel.removeEventListener("message", this.#message);
    this.#channel.removeEventListener("close", this.#closed);
    this.#video.style.removeProperty("cursor");
    delete this.#video.dataset.cursorMode;
  }
  #closed = (): void => { if (this.#enabled) this.#video.style.cursor = "default"; };
  #message = (event: MessageEvent): void => {
    if (!this.#enabled || typeof event.data !== "string" || event.data.length > 101_000) return;
    try { const style = cursorStyle(JSON.parse(event.data)); if (style) this.#video.style.cursor = style; } catch { /* malformed cursor metadata */ }
  };
}
