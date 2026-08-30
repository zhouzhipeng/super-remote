import { websocketTicket } from "./api.ts";
import type { ClientSignal, ServerSignal } from "./types.ts";

export class SignalingSocket extends EventTarget {
  #socket: WebSocket | null = null;

  async connect(): Promise<void> {
    const ticket = await websocketTicket();
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(`${scheme}//${location.host}/api/ws?ticket=${encodeURIComponent(ticket)}`);
    this.#socket = socket;
    await new Promise<void>((resolve, reject) => {
      socket.onopen = () => resolve();
      socket.onerror = () => reject(new Error("signaling websocket failed"));
    });
    socket.onmessage = (event) => {
      try {
        const signal = JSON.parse(String(event.data)) as ServerSignal;
        this.dispatchEvent(new CustomEvent<ServerSignal>("signal", { detail: signal }));
      } catch (error) {
        console.error("invalid signaling message", error);
      }
    };
    socket.onclose = () => this.dispatchEvent(new Event("close"));
  }

  send(signal: ClientSignal): void {
    if (this.#socket?.readyState !== WebSocket.OPEN) throw new Error("signaling is not connected");
    this.#socket.send(JSON.stringify(signal));
  }

  close(): void {
    this.#socket?.close();
    this.#socket = null;
  }
}
