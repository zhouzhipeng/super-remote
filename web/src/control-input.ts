import type { InputTransport } from "./input.ts";
import type { SignalingSocket } from "./signaling.ts";
import type { ServerSignal } from "./types.ts";

// Authenticated control socket never carries video/audio. In particular, input
// does not wait behind TURN/TCP media bytes or SCTP retransmissions.
export class ControlInputChannel extends EventTarget implements InputTransport {
  bufferedAmountLowThreshold = 0;
  #socket: SignalingSocket;
  #session: string;
  #closed = false;
  #timer: ReturnType<typeof setTimeout> | null = null;
  constructor(socket: SignalingSocket, session: string) {
    super(); this.#socket = socket; this.#session = session;
    socket.addEventListener("signal", this.#onSignal);
  }
  get readyState(): string { return this.#closed ? "closed" : this.#socket.readyState; }
  get bufferedAmount(): number {
    const amount = this.#socket.bufferedAmount;
    if (amount > 0) this.#watchDrain();
    return amount;
  }
  send(data: ArrayBufferView<ArrayBuffer>): void {
    if (this.readyState !== "open") return;
    this.#socket.send({ type: "input_packet", session_id: this.#session,
      data: Array.from(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)) });
  }
  close(): void {
    this.#closed = true;
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#socket.removeEventListener("signal", this.#onSignal);
  }
  #watchDrain(): void {
    if (this.#closed || this.#timer !== null) return;
    this.#timer = setTimeout(() => {
      this.#timer = null;
      if (this.readyState !== "open") return;
      if (this.#socket.bufferedAmount > 0) this.#watchDrain();
      else this.dispatchEvent(new Event("bufferedamountlow"));
    }, 4);
  }
  #onSignal = (event: Event): void => {
    const signal = (event as CustomEvent<ServerSignal>).detail;
    if (signal.type !== "input_ack" || signal.session_id !== this.#session) return;
    this.dispatchEvent(new MessageEvent("message", { data: Uint8Array.from(signal.data).buffer }));
  };
}
