import { browserKeyboardProfile, remoteScanCode, type KeyboardProfile } from "./keymap.ts";
import { ClipboardShortcutRouter } from "./clipboard-shortcuts.ts";

const HEADER_LENGTH = 12;
const ACK_REQUESTED = 0x01;
// At most one tiny input message may wait in SCTP. Larger limits make the
// remote pointer replay stale positions after a transient network stall.
const FAST_BUFFER_LIMIT = 16;

enum InputType {
  MouseMove = 0x01,
  MouseButton = 0x02,
  Keyboard = 0x03,
  MouseWheel = 0x04,
  MouseRelative = 0x05,
}

function packet(type: InputType, payloadLength: number, flags = 0): DataView {
  const view = new DataView(new ArrayBuffer(HEADER_LENGTH + payloadLength));
  view.setUint8(0, type);
  view.setUint8(1, flags);
  view.setUint16(2, payloadLength, true);
  view.setBigUint64(4, BigInt(Math.round(performance.timeOrigin * 1000 + performance.now() * 1000)), true);
  return view;
}

function clamp16(value: number): number {
  return Math.max(0, Math.min(65535, Math.round(value)));
}

export class InputController {
  #video: HTMLVideoElement;
  #fast: RTCDataChannel;
  #reliable: RTCDataChannel;
  #pendingMove: { x: number; y: number } | null = null;
  #lastRawUpdate = -Infinity;
  #moveSequence = 0;
  #pressed = new Set<string>();
  #clipboardShortcuts = new ClipboardShortcutRouter();
  #pasteSink: HTMLTextAreaElement;
  #pasteAttempt = 0;
  #pasteHandled = 0;
  #pasteFocusTimer = 0;
  #keyboardProfile: KeyboardProfile;

  constructor(
    video: HTMLVideoElement,
    fast: RTCDataChannel,
    reliable: RTCDataChannel,
    private readonly onLatency: (milliseconds: number) => void,
    private readonly onPasteText: (text: string) => void,
    private readonly onCopyShortcut: () => void,
  ) {
    this.#video = video;
    this.#fast = fast;
    this.#reliable = reliable;
    this.#keyboardProfile = browserKeyboardProfile();
    this.#pasteSink = document.createElement("textarea");
    this.#pasteSink.className = "remote-clipboard-sink";
    this.#pasteSink.tabIndex = -1;
    this.#pasteSink.setAttribute("aria-hidden", "true");
    this.#pasteSink.setAttribute("autocomplete", "off");
    this.#pasteSink.setAttribute("autocapitalize", "off");
    this.#pasteSink.spellcheck = false;
    (video.parentElement ?? document.body).append(this.#pasteSink);
    video.dataset.keyboardProfile = this.#keyboardProfile;
    video.dataset.inputMode = "pointermove";
    // Register the raw event unconditionally: browsers that do not implement it
    // simply never emit it. pointermove remains the Safari/mobile fallback.
    video.addEventListener("pointerrawupdate", this.#pointerRawUpdate as EventListener);
    video.addEventListener("pointermove", this.#pointerMove);
    video.addEventListener("pointerdown", this.#pointerButton);
    video.addEventListener("pointerup", this.#pointerButton);
    video.addEventListener("contextmenu", this.#prevent);
    video.addEventListener("wheel", this.#wheel, { passive: false });
    window.addEventListener("keydown", this.#keyboard, true);
    window.addEventListener("keyup", this.#keyboard, true);
    window.addEventListener("paste", this.#paste, true);
    this.#pasteSink.addEventListener("input", this.#pasteSinkInput);
    window.addEventListener("blur", this.#releaseAll);
    fast.bufferedAmountLowThreshold = 0;
    fast.addEventListener("bufferedamountlow", this.#flushPendingMove);
    fast.addEventListener("message", this.#inputAck);
    reliable.addEventListener("message", this.#inputAck);
  }

  destroy(): void {
    this.#video.removeEventListener("pointerrawupdate", this.#pointerRawUpdate as EventListener);
    this.#video.removeEventListener("pointermove", this.#pointerMove);
    this.#video.removeEventListener("pointerdown", this.#pointerButton);
    this.#video.removeEventListener("pointerup", this.#pointerButton);
    this.#video.removeEventListener("contextmenu", this.#prevent);
    this.#video.removeEventListener("wheel", this.#wheel);
    window.removeEventListener("keydown", this.#keyboard, true);
    window.removeEventListener("keyup", this.#keyboard, true);
    window.removeEventListener("paste", this.#paste, true);
    this.#pasteSink.removeEventListener("input", this.#pasteSinkInput);
    window.removeEventListener("blur", this.#releaseAll);
    this.#fast.removeEventListener("bufferedamountlow", this.#flushPendingMove);
    this.#fast.removeEventListener("message", this.#inputAck);
    this.#reliable.removeEventListener("message", this.#inputAck);
    window.clearTimeout(this.#pasteFocusTimer);
    this.#pasteSink.remove();
    this.#releaseAll();
  }

  #prevent = (event: Event): void => event.preventDefault();

  #pointerMove = (event: PointerEvent): void => {
    if (performance.now() - this.#lastRawUpdate < 32) return;
    this.#sendPointerEvent(event);
  };

  #pointerRawUpdate = (event: PointerEvent): void => {
    this.#lastRawUpdate = performance.now();
    this.#video.dataset.inputMode = "pointerrawupdate";
    this.#sendPointerEvent(event);
  };

  #sendPointerEvent(event: PointerEvent): void {
    const coalesced = event.getCoalescedEvents?.();
    const latest = coalesced?.[coalesced.length - 1] ?? event;
    const point = this.#normalizedPoint(latest.clientX, latest.clientY);
    if (!point) return;
    this.#sendFastMove(point);
  }

  #flushPendingMove = (): void => {
    const move = this.#pendingMove;
    this.#pendingMove = null;
    if (move) this.#sendFastMove(move);
  };

  #sendFastMove(move: { x: number; y: number }): void {
    if (this.#fast.readyState !== "open") return;
    if (this.#fast.bufferedAmount >= FAST_BUFFER_LIMIT) {
      this.#pendingMove = move;
      return;
    }
    this.#moveSequence += 1;
    const flags = this.#moveSequence % 16 === 0 ? ACK_REQUESTED : 0;
    const view = packet(InputType.MouseMove, 4, flags);
    view.setUint16(12, move.x, true);
    view.setUint16(14, move.y, true);
    this.#fast.send(bytes(view));
  }

  #pointerButton = (event: PointerEvent): void => {
    event.preventDefault();
    this.#video.focus();
    const point = this.#normalizedPoint(event.clientX, event.clientY);
    if (!point) return;
    if (event.type === "pointerdown") this.#video.setPointerCapture?.(event.pointerId);
    const view = packet(InputType.MouseButton, 6, ACK_REQUESTED);
    view.setUint16(12, point.x, true);
    view.setUint16(14, point.y, true);
    view.setUint8(16, event.button);
    view.setUint8(17, event.type === "pointerdown" ? 1 : 0);
    this.#sendReliable(bytes(view));
  };

  #wheel = (event: WheelEvent): void => {
    event.preventDefault();
    const view = packet(InputType.MouseWheel, 4);
    view.setInt16(12, clamp16Signed(-event.deltaX), true);
    view.setInt16(14, clamp16Signed(-event.deltaY), true);
    if (this.#fast.readyState === "open" && this.#fast.bufferedAmount <= FAST_BUFFER_LIMIT) this.#fast.send(bytes(view));
  };

  #keyboard = (event: KeyboardEvent): void => {
    if (isLocalUiTarget(event.target)) return;
    const mapping = remoteScanCode(event.code, this.#keyboardProfile);
    if (!mapping) return;
    const clipboardModifier = event.ctrlKey || event.metaKey;
    if (event.code === "KeyV" && (clipboardModifier || this.#clipboardShortcuts.pasteActive)) {
      const pasteRoute = event.type === "keydown"
        ? this.#clipboardShortcuts.beginPaste()
        : this.#clipboardShortcuts.endPaste();
      if (pasteRoute === "browser") {
        // A video is not editable, so Safari and some Chromium configurations
        // never dispatch paste to it. Move focus to an off-screen textarea before
        // the browser runs the shortcut's default action. ClipboardEvent remains
        // the HTTP-compatible path; the async API is an HTTPS fallback.
        if (event.type === "keydown" && !event.repeat) this.#captureBrowserPaste();
        return;
      }
      // A copy/cut performed in this still-focused remote session already put
      // the right text on the Host clipboard. Forward V normally instead of
      // overwriting that text with a stale browser clipboard.
    }
    event.preventDefault();
    event.stopPropagation();
    const down = event.type === "keydown";
    if (down && this.#pressed.has(event.code) && !event.repeat) return;
    if (down) this.#pressed.add(event.code); else this.#pressed.delete(event.code);
    this.#sendKey(mapping[0], down, mapping[1]);
    if (!down && clipboardModifier && (event.code === "KeyC" || event.code === "KeyX")) {
      this.#clipboardShortcuts.markRemoteCopy();
      this.onCopyShortcut();
    }
  };

  #paste = (event: ClipboardEvent): void => {
    if (isLocalUiTarget(event.target)) return;
    if (!event.clipboardData) return;
    const text = event.clipboardData.getData("text/plain");
    if (!text && document.activeElement === this.#pasteSink) {
      // Safari can expose an empty ClipboardEvent while still inserting the
      // real text into the focused textarea as the event's default action.
      // Do not clear the Host clipboard or prevent the follow-up input event.
      this.#video.dataset.clipboardPasteSource = "waiting-editable";
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const attempt = this.#clipboardShortcuts.pasteActive ? this.#pasteAttempt : ++this.#pasteAttempt;
    this.#deliverBrowserPaste(text, "event", attempt);
  };

  #pasteSinkInput = (): void => {
    this.#deliverBrowserPaste(this.#pasteSink.value, "editable");
  };

  #captureBrowserPaste(): void {
    this.#pasteAttempt += 1;
    const attempt = this.#pasteAttempt;
    this.#pasteSink.value = "";
    this.#pasteSink.focus({ preventScroll: true });
    this.#pasteSink.select();
    this.#video.dataset.clipboardPasteSource = "waiting";
    window.clearTimeout(this.#pasteFocusTimer);
    this.#pasteFocusTimer = window.setTimeout(() => {
      if (document.activeElement === this.#pasteSink) this.#video.focus({ preventScroll: true });
    }, 300);
    if (navigator.clipboard?.readText) {
      void navigator.clipboard.readText().then((text) => {
        if (this.#pasteHandled < attempt) this.#deliverBrowserPaste(text, "api", attempt);
      }).catch(() => undefined);
    }
  }

  #deliverBrowserPaste(text: string, source: "event" | "editable" | "api", attempt = this.#pasteAttempt): void {
    if (attempt > 0 && this.#pasteHandled >= attempt) return;
    this.#pasteHandled = Math.max(this.#pasteHandled, attempt);
    window.clearTimeout(this.#pasteFocusTimer);
    this.#pasteSink.value = "";
    this.#video.dataset.clipboardPasteSource = source;
    this.onPasteText(text);
    this.#video.focus({ preventScroll: true });
  }

  #sendKey(scanCode: number, down: boolean, extended: boolean): void {
    const view = packet(InputType.Keyboard, 4, ACK_REQUESTED);
    view.setUint16(12, scanCode, true);
    view.setUint8(14, down ? 1 : 0);
    view.setUint8(15, extended ? 1 : 0);
    this.#sendReliable(bytes(view));
  }

  #releaseAll = (): void => {
    for (const code of this.#pressed) {
      const mapping = remoteScanCode(code, this.#keyboardProfile);
      if (mapping) this.#sendKey(mapping[0], false, mapping[1]);
    }
    this.#pressed.clear();
    this.#clipboardShortcuts.reset();
    window.clearTimeout(this.#pasteFocusTimer);
  };

  #sendReliable(data: ArrayBufferView<ArrayBuffer>): void {
    if (this.#reliable.readyState === "open") this.#reliable.send(data);
  }

  #inputAck = (event: MessageEvent<ArrayBuffer>): void => {
    if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < HEADER_LENGTH) return;
    const view = new DataView(event.data);
    if ((view.getUint8(1) & ACK_REQUESTED) === 0) return;
    const sentUs = Number(view.getBigUint64(4, true));
    const nowUs = performance.timeOrigin * 1000 + performance.now() * 1000;
    const latency = Math.max(0, (nowUs - sentUs) / 1000);
    this.#video.dataset.inputRttMs = latency.toFixed(3);
    this.#video.dataset.inputAckCount = String(Number(this.#video.dataset.inputAckCount ?? "0") + 1);
    this.onLatency(latency);
  };

  #normalizedPoint(clientX: number, clientY: number): { x: number; y: number } | null {
    const rect = this.#video.getBoundingClientRect();
    const sourceAspect = this.#video.videoWidth / this.#video.videoHeight;
    if (!Number.isFinite(sourceAspect) || sourceAspect <= 0) return null;
    const boxAspect = rect.width / rect.height;
    const displayWidth = boxAspect > sourceAspect ? rect.height * sourceAspect : rect.width;
    const displayHeight = boxAspect > sourceAspect ? rect.height : rect.width / sourceAspect;
    const left = rect.left + (rect.width - displayWidth) / 2;
    const top = rect.top + (rect.height - displayHeight) / 2;
    if (clientX < left || clientX > left + displayWidth || clientY < top || clientY > top + displayHeight) return null;
    return { x: clamp16(((clientX - left) / displayWidth) * 65535), y: clamp16(((clientY - top) / displayHeight) * 65535) };
  }
}

function isLocalUiTarget(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(".toolbar, .clipboard-panel") !== null;
}

function bytes(view: DataView): Uint8Array<ArrayBuffer> {
  return new Uint8Array(view.buffer as ArrayBuffer);
}

function clamp16Signed(value: number): number {
  return Math.max(-32768, Math.min(32767, Math.round(value)));
}
