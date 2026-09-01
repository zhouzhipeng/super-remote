import { createSession, iceServers, reportClient } from "./api.ts";
import { InputController } from "./input.ts";
import { SignalingSocket } from "./signaling.ts";
import { StatsMonitor } from "./stats.ts";
import type { ServerSignal } from "./types.ts";

export type SessionState = "idle" | "creating_session" | "negotiating" | "connected" | "reconnecting" | "closed";
export type ClipboardSyncDetail = { text: string; copied: boolean; automatic: boolean };

type ClipboardResponse =
  | { type: "content"; id: number; text: string }
  | { type: "ack"; id: number }
  | { type: "error"; id: number; message: string };

const MAX_CLIPBOARD_TEXT_BYTES = 12 * 1024;

export class RemoteSession extends EventTarget {
  state: SessionState = "idle";
  #peer: RTCPeerConnection | null = null;
  #signaling = new SignalingSocket();
  #sessionId = "";
  #sessionToken = "";
  #pendingIce: RTCIceCandidateInit[] = [];
  #input: InputController | null = null;
  #stats: StatsMonitor | null = null;
  #reportTimers: number[] = [];
  #remoteStream = new MediaStream();
  #clipboard: RTCDataChannel | null = null;
  #clipboardRequestId = 0;
  #clipboardRequests = new Map<number, { resolve: (text: string) => void; reject: (error: Error) => void; timer: number }>();
  #clipboardPullTimer = 0;

  constructor(private readonly video: HTMLVideoElement, private readonly statsOutput: HTMLElement) { super(); }

  async connect(deviceId: string): Promise<void> {
    this.#setState("creating_session");
    await this.#signaling.connect();
    this.#signaling.addEventListener("signal", this.#onSignal);
    const { session_id, session_token } = await createSession(deviceId);
    this.#sessionId = session_id;
    this.#sessionToken = session_token;
    this.#setState("negotiating");

    const peer = new RTCPeerConnection({ iceServers: await iceServers(), bundlePolicy: "max-bundle" });
    this.#peer = peer;
    peer.addTransceiver("video", { direction: "recvonly" });
    peer.addTransceiver("audio", { direction: "recvonly" });
    const fast = peer.createDataChannel("input-fast", { ordered: false, maxRetransmits: 0 });
    const reliable = peer.createDataChannel("input-reliable", { ordered: true });
    const clipboard = peer.createDataChannel("clipboard", { ordered: true });
    this.#clipboard = clipboard;
    fast.binaryType = reliable.binaryType = "arraybuffer";
    clipboard.addEventListener("message", this.#onClipboardMessage);
    clipboard.addEventListener("open", () => this.dispatchEvent(new Event("clipboardready")));
    reliable.addEventListener("open", () => {
      this.#input = new InputController(
        this.video,
        fast,
        reliable,
        (latency) => this.#stats?.setInputLatency(latency),
        (text) => {
          void this.writeClipboard(text, true).catch((error) => this.#clipboardError(error));
        },
        () => {
          window.clearTimeout(this.#clipboardPullTimer);
          this.#clipboardPullTimer = window.setTimeout(() => {
            void this.syncHostClipboardToBrowser(true).catch((error) => this.#clipboardError(error));
          }, 120);
        },
      );
    });
    this.video.muted = true;
    this.video.playsInline = true;
    for (const name of ["loadedmetadata", "canplay", "playing", "waiting", "stalled", "error"] as const) {
      this.video.addEventListener(name, () => { void this.#report(`media:${name}`); });
    }
    peer.ontrack = (event) => {
      if (!this.#remoteStream.getTracks().some((track) => track.id === event.track.id)) {
        this.#remoteStream.addTrack(event.track);
      }
      this.video.srcObject = this.#remoteStream;
      const play = () => { void this.video.play().catch(() => {
        this.dispatchEvent(new CustomEvent("error", { detail: "请点击“开启声音”" }));
      }); };
      if (this.video.readyState >= HTMLMediaElement.HAVE_METADATA) play();
      else this.video.addEventListener("loadedmetadata", play, { once: true });
    };
    peer.onicecandidate = ({ candidate }) => {
      if (!candidate) return;
      this.#signaling.send({
        type: "webrtc_ice", session_id: this.#sessionId, candidate: candidate.candidate,
        sdp_mid: candidate.sdpMid, sdp_mline_index: candidate.sdpMLineIndex, username_fragment: candidate.usernameFragment,
      });
    };
    peer.onconnectionstatechange = () => {
      if (peer.connectionState === "connected") {
        this.#setState("connected");
        this.#stats = new StatsMonitor(peer, this.statsOutput);
        this.#stats.start();
        for (const delay of [0, 2000, 5000, 15000]) {
          this.#reportTimers.push(window.setTimeout(() => { void this.#report(`connected:${delay}`); }, delay));
        }
      } else if (peer.connectionState === "disconnected") this.#setState("reconnecting");
      else if (peer.connectionState === "failed" || peer.connectionState === "closed") this.close();
    };
    const offer = await peer.createOffer();
    await peer.setLocalDescription(offer);
    const viewport = this.#physicalVideoArea();
    this.#signaling.send({
      type: "webrtc_offer", session_id: this.#sessionId, session_token: this.#sessionToken,
      sdp: offer.sdp ?? "", viewport_width: viewport.width, viewport_height: viewport.height,
    });
  }

  setMuted(muted: boolean): Promise<void> {
    this.video.muted = muted;
    return this.video.play();
  }

  readClipboard(): Promise<string> {
    return this.#clipboardRequest({ type: "read" });
  }

  writeClipboard(text: string, paste = false): Promise<string> {
    if (new TextEncoder().encode(text).byteLength > MAX_CLIPBOARD_TEXT_BYTES) {
      return Promise.reject(new Error(`剪贴板文本不能超过 ${MAX_CLIPBOARD_TEXT_BYTES / 1024} KiB`));
    }
    return this.#clipboardRequest({ type: "write", text, paste });
  }

  async syncHostClipboardToBrowser(automatic = false): Promise<ClipboardSyncDetail> {
    const text = await this.readClipboard();
    let copied = false;
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
    } catch { /* Plain HTTP falls through to the hidden native-copy bridge below. */ }
    if (!copied) copied = legacyCopyText(text);
    const detail = { text, copied, automatic };
    this.dispatchEvent(new CustomEvent<ClipboardSyncDetail>("clipboard", { detail }));
    return detail;
  }

  close(): void {
    if (this.state === "closed") return;
    if (this.#sessionId) {
      try { this.#signaling.send({ type: "session_close", session_id: this.#sessionId }); } catch { /* already disconnected */ }
    }
    this.#input?.destroy();
    window.clearTimeout(this.#clipboardPullTimer);
    this.#clipboard?.removeEventListener("message", this.#onClipboardMessage);
    this.#clipboard = null;
    for (const request of this.#clipboardRequests.values()) {
      window.clearTimeout(request.timer);
      request.reject(new Error("远程会话已关闭"));
    }
    this.#clipboardRequests.clear();
    this.#stats?.stop();
    for (const timer of this.#reportTimers.splice(0)) window.clearTimeout(timer);
    this.#peer?.close();
    this.#signaling.close();
    this.video.srcObject = null;
    this.#setState("closed");
  }

  #onSignal = (event: Event): void => {
    void this.#handleSignal((event as CustomEvent<ServerSignal>).detail);
  };

  #clipboardRequest(message: { type: "read" } | { type: "write"; text: string; paste: boolean }): Promise<string> {
    const channel = this.#clipboard;
    if (!channel || channel.readyState !== "open") return Promise.reject(new Error("剪贴板通道尚未连接"));
    const id = ++this.#clipboardRequestId;
    const payload = JSON.stringify({ ...message, id });
    return new Promise<string>((resolve, reject) => {
      const timer = window.setTimeout(() => {
        this.#clipboardRequests.delete(id);
        reject(new Error("剪贴板操作超时"));
      }, 5000);
      this.#clipboardRequests.set(id, { resolve, reject, timer });
      try {
        channel.send(payload);
      } catch (error) {
        window.clearTimeout(timer);
        this.#clipboardRequests.delete(id);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  #onClipboardMessage = (event: MessageEvent<string>): void => {
    if (typeof event.data !== "string") return;
    let response: ClipboardResponse;
    try { response = JSON.parse(event.data) as ClipboardResponse; } catch { return; }
    const request = this.#clipboardRequests.get(response.id);
    if (!request) return;
    window.clearTimeout(request.timer);
    this.#clipboardRequests.delete(response.id);
    if (response.type === "error") request.reject(new Error(response.message));
    else request.resolve(response.type === "content" ? response.text : "");
  };

  #clipboardError(error: unknown): void {
    this.dispatchEvent(new CustomEvent("error", {
      detail: error instanceof Error ? error.message : String(error),
    }));
  }

  async #handleSignal(signal: ServerSignal): Promise<void> {
    if ("session_id" in signal && signal.session_id !== this.#sessionId) return;
    if (signal.type === "webrtc_answer") {
      await this.#peer?.setRemoteDescription({ type: "answer", sdp: signal.sdp });
      this.#requestLowLatencyPlayback();
      for (const candidate of this.#pendingIce.splice(0)) await this.#peer?.addIceCandidate(candidate);
    }
    if (signal.type === "webrtc_ice") {
      const candidate = { candidate: signal.candidate, sdpMid: signal.sdp_mid, sdpMLineIndex: signal.sdp_mline_index, usernameFragment: signal.username_fragment ?? undefined };
      if (this.#peer?.remoteDescription) await this.#peer.addIceCandidate(candidate);
      else this.#pendingIce.push(candidate);
    }
    if (signal.type === "session_closed") this.close();
    if (signal.type === "error") this.dispatchEvent(new CustomEvent("error", { detail: signal.message }));
  }

  #setState(state: SessionState): void {
    this.state = state;
    this.dispatchEvent(new CustomEvent<SessionState>("state", { detail: state }));
  }

  #requestLowLatencyPlayback(): void {
    type LowLatencyReceiver = RTCRtpReceiver & {
      jitterBufferTarget?: number;
      playoutDelayHint?: number;
    };
    for (const receiver of this.#peer?.getReceivers() ?? []) {
      if (receiver.track?.kind !== "video") continue;
      const lowLatency = receiver as LowLatencyReceiver;
      try { lowLatency.jitterBufferTarget = 0; } catch { /* browser-controlled fallback */ }
      try { lowLatency.playoutDelayHint = 0; } catch { /* older browser fallback */ }
    }
  }

  #physicalVideoArea(): { width: number; height: number } {
    const rect = this.video.getBoundingClientRect();
    const pixelRatio = Math.max(1, window.devicePixelRatio || 1);
    return {
      width: Math.max(2, Math.floor(rect.width * pixelRatio)),
      height: Math.max(2, Math.floor(rect.height * pixelRatio)),
    };
  }

  async #report(stage: string): Promise<void> {
    const inbound = this.#peer ? [...(await this.#peer.getStats()).values()].find(
      (item) => item.type === "inbound-rtp" && item.kind === "video",
    ) : undefined;
    const audioInbound = this.#peer ? [...(await this.#peer.getStats()).values()].find(
      (item) => item.type === "inbound-rtp" && item.kind === "audio",
    ) : undefined;
    const quality = this.video.getVideoPlaybackQuality?.();
    await reportClient({
      stage,
      userAgent: navigator.userAgent,
      visibility: document.visibilityState,
      readyState: this.video.readyState,
      paused: this.video.paused,
      muted: this.video.muted,
      audioTracks: this.#remoteStream.getAudioTracks().length,
      audioBytesReceived: audioInbound?.bytesReceived ?? null,
      audioPacketsReceived: audioInbound?.packetsReceived ?? null,
      videoWidth: this.video.videoWidth,
      videoHeight: this.video.videoHeight,
      currentTime: this.video.currentTime,
      framesReceived: inbound?.framesReceived ?? null,
      framesDecoded: inbound?.framesDecoded ?? null,
      keyFramesDecoded: inbound?.keyFramesDecoded ?? null,
      bytesReceived: inbound?.bytesReceived ?? null,
      packetsLost: inbound?.packetsLost ?? null,
      framesDropped: quality?.droppedVideoFrames ?? null,
      mediaError: this.video.error?.message ?? null,
    }).catch(() => undefined);
  }
}

function legacyCopyText(text: string): boolean {
  const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const sink = document.createElement("textarea");
  sink.className = "remote-clipboard-sink";
  sink.tabIndex = -1;
  sink.setAttribute("aria-hidden", "true");
  sink.value = text;
  document.body.append(sink);
  sink.focus({ preventScroll: true });
  sink.select();
  let copied = false;
  try { copied = document.execCommand("copy"); } catch { /* browser denied the legacy fallback */ }
  sink.remove();
  previousFocus?.focus({ preventScroll: true });
  return copied;
}
