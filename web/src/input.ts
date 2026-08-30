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

const SCAN_CODES: Record<string, [number, boolean]> = {
  Escape: [0x01, false], Digit1: [0x02, false], Digit2: [0x03, false], Digit3: [0x04, false],
  Digit4: [0x05, false], Digit5: [0x06, false], Digit6: [0x07, false], Digit7: [0x08, false],
  Digit8: [0x09, false], Digit9: [0x0a, false], Digit0: [0x0b, false], Minus: [0x0c, false], Equal: [0x0d, false],
  Backspace: [0x0e, false], Tab: [0x0f, false], KeyQ: [0x10, false], KeyW: [0x11, false], KeyE: [0x12, false],
  KeyR: [0x13, false], KeyT: [0x14, false], KeyY: [0x15, false], KeyU: [0x16, false], KeyI: [0x17, false],
  KeyO: [0x18, false], KeyP: [0x19, false], BracketLeft: [0x1a, false], BracketRight: [0x1b, false],
  Enter: [0x1c, false], ControlLeft: [0x1d, false], KeyA: [0x1e, false], KeyS: [0x1f, false],
  KeyD: [0x20, false], KeyF: [0x21, false], KeyG: [0x22, false], KeyH: [0x23, false], KeyJ: [0x24, false],
  KeyK: [0x25, false], KeyL: [0x26, false], Semicolon: [0x27, false], Quote: [0x28, false], Backquote: [0x29, false],
  ShiftLeft: [0x2a, false], Backslash: [0x2b, false], KeyZ: [0x2c, false], KeyX: [0x2d, false],
  KeyC: [0x2e, false], KeyV: [0x2f, false], KeyB: [0x30, false], KeyN: [0x31, false], KeyM: [0x32, false],
  Comma: [0x33, false], Period: [0x34, false], Slash: [0x35, false], ShiftRight: [0x36, false],
  AltLeft: [0x38, false], Space: [0x39, false], CapsLock: [0x3a, false],
  F1: [0x3b, false], F2: [0x3c, false], F3: [0x3d, false], F4: [0x3e, false], F5: [0x3f, false], F6: [0x40, false],
  F7: [0x41, false], F8: [0x42, false], F9: [0x43, false], F10: [0x44, false], F11: [0x57, false], F12: [0x58, false],
  ControlRight: [0x1d, true], AltRight: [0x38, true], Home: [0x47, true], ArrowUp: [0x48, true],
  PageUp: [0x49, true], ArrowLeft: [0x4b, true], ArrowRight: [0x4d, true], End: [0x4f, true],
  ArrowDown: [0x50, true], PageDown: [0x51, true], Insert: [0x52, true], Delete: [0x53, true],
  MetaLeft: [0x5b, true], MetaRight: [0x5c, true],
};

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

  constructor(
    video: HTMLVideoElement,
    fast: RTCDataChannel,
    reliable: RTCDataChannel,
    private readonly onLatency: (milliseconds: number) => void,
  ) {
    this.#video = video;
    this.#fast = fast;
    this.#reliable = reliable;
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
    window.removeEventListener("blur", this.#releaseAll);
    this.#fast.removeEventListener("bufferedamountlow", this.#flushPendingMove);
    this.#fast.removeEventListener("message", this.#inputAck);
    this.#reliable.removeEventListener("message", this.#inputAck);
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
    const mapping = SCAN_CODES[event.code];
    if (!mapping) return;
    event.preventDefault();
    event.stopPropagation();
    const down = event.type === "keydown";
    if (down && this.#pressed.has(event.code) && !event.repeat) return;
    if (down) this.#pressed.add(event.code); else this.#pressed.delete(event.code);
    this.#sendKey(mapping[0], down, mapping[1]);
  };

  #sendKey(scanCode: number, down: boolean, extended: boolean): void {
    const view = packet(InputType.Keyboard, 4, ACK_REQUESTED);
    view.setUint16(12, scanCode, true);
    view.setUint8(14, down ? 1 : 0);
    view.setUint8(15, extended ? 1 : 0);
    this.#sendReliable(bytes(view));
  }

  #releaseAll = (): void => {
    for (const code of this.#pressed) {
      const mapping = SCAN_CODES[code];
      if (mapping) this.#sendKey(mapping[0], false, mapping[1]);
    }
    this.#pressed.clear();
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

function bytes(view: DataView): Uint8Array<ArrayBuffer> {
  return new Uint8Array(view.buffer as ArrayBuffer);
}

function clamp16Signed(value: number): number {
  return Math.max(-32768, Math.min(32767, Math.round(value)));
}
