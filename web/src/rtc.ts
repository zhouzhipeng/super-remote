import { createSession, iceServers, localClipboardTextDuringGesture, reportClient } from "./api.ts";
import type { ConnectionPhase } from "./connection-progress.ts";
import { chromiumCompatibleIceServers, remoteIceCandidate, shouldUseChromiumLanCompatibility } from "./ice.ts";
import { InputController } from "./input.ts";
import { disconnectMessage } from "./session-close.ts";
import { SignalingSocket } from "./signaling.ts";
import { StatsMonitor } from "./stats.ts";
import type { ServerSignal } from "./types.ts";

export type SessionState = "idle" | "creating_session" | "negotiating" | "connected" | "reconnecting" | "closed";
export type ClipboardSyncDetail = { text: string; copied: boolean; automatic: boolean };
export type SessionDisconnectDetail = { message: string };
export type ConnectionProgressDetail = { phase: ConnectionPhase; detail?: string };

type ClipboardResponse =
  | { type: "content"; id: number; text: string }
  | { type: "ack"; id: number }
  | { type: "error"; id: number; message: string };

type LocalIceSignal = {
  type: "webrtc_ice";
  session_id: string;
  candidate: string;
  sdp_mid: string | null;
  sdp_mline_index: number | null;
  username_fragment: string | null;
};

const MAX_CLIPBOARD_TEXT_BYTES = 12 * 1024;

export class RemoteSession extends EventTarget {
  state: SessionState = "idle";
  #peer: RTCPeerConnection | null = null;
  #signaling = new SignalingSocket();
  #sessionId = "";
  #sessionToken = "";
  #pendingIce: RTCIceCandidateInit[] = [];
  #pendingLocalIce: LocalIceSignal[] = [];
  #offerSent = false;
  #localCandidateCount = 0;
  #remoteCandidateCount = 0;
  #input: InputController | null = null;
  #stats: StatsMonitor | null = null;
  #reportTimers: number[] = [];
  #remoteStream = new MediaStream();
  #clipboard: RTCDataChannel | null = null;
  #clipboardRequestId = 0;
  #clipboardRequests = new Map<number, { resolve: (text: string) => void; reject: (error: Error) => void; timer: number }>();
  #clipboardPullTimer = 0;
  #closing = false;
  #disconnectReported = false;

  constructor(private readonly video: HTMLVideoElement, private readonly statsOutput: HTMLElement) { super(); }

  async connect(deviceId: string): Promise<void> {
    this.#progress("signaling");
    this.#setState("creating_session");
    this.#signaling.addEventListener("close", this.#onSignalingClose);
    await this.#signaling.connect();
    this.#signaling.addEventListener("signal", this.#onSignal);
    const configuredIceServers = await iceServers();
    const chromiumLanCompatibility = shouldUseChromiumLanCompatibility(navigator.userAgent);
    if (chromiumLanCompatibility) {
      this.#progress("candidates", "已识别 Chrome，正在同时探测直连、STUN 与 TURN 路径");
    }
    const peerIceServers = chromiumLanCompatibility
      ? chromiumCompatibleIceServers(configuredIceServers)
      : configuredIceServers;
    const peer = new RTCPeerConnection({
      iceServers: peerIceServers,
      // Chrome 151 on this Mac previously connected through its mDNS host
      // candidate. Forcing `relay` removed that known-good LAN path while the
      // macOS Chrome network service produced no private TURN candidates at
      // all. Keep TURN configured as fallback, but restore host candidates.
      iceTransportPolicy: "all",
      bundlePolicy: "max-bundle",
    });
    this.#peer = peer;
    let resolveFirstCandidate!: () => void;
    let rejectFirstCandidate!: (error: Error) => void;
    let firstCandidateSettled = false;
    const firstCandidate = new Promise<void>((resolve, reject) => {
      resolveFirstCandidate = resolve;
      rejectFirstCandidate = reject;
    });
    const candidateReady = (): void => {
      if (firstCandidateSettled) return;
      firstCandidateSettled = true;
      resolveFirstCandidate();
    };
    const candidateFailed = (message: string): void => {
      if (firstCandidateSettled) return;
      firstCandidateSettled = true;
      rejectFirstCandidate(new Error(message));
    };
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
          void this.pasteHostClipboard().catch((error) => this.#clipboardError(error));
        },
        () => localClipboardTextDuringGesture(),
      );
    });
    this.video.muted = true;
    this.video.playsInline = true;
    for (const name of ["loadedmetadata", "canplay", "playing", "waiting", "stalled", "error"] as const) {
      this.video.addEventListener(name, this.#onMediaEvent);
    }
    peer.ontrack = (event) => {
      if (!this.#remoteStream.getTracks().some((track) => track.id === event.track.id)) {
        this.#remoteStream.addTrack(event.track);
      }
      this.video.srcObject = this.#remoteStream;
      this.#progress("video", `已收到${event.track.kind === "video" ? "视频" : "音频"}轨道，正在等待首帧`);
      const play = () => { void this.video.play().catch(() => {
        this.dispatchEvent(new CustomEvent("error", { detail: "请点击“开启声音”" }));
      }); };
      if (this.video.readyState >= HTMLMediaElement.HAVE_METADATA) play();
      else this.video.addEventListener("loadedmetadata", play, { once: true });
    };
    peer.onicecandidate = ({ candidate }) => {
      if (!candidate) return;
      this.#localCandidateCount += 1;
      candidateReady();
      this.#progress("candidates", `已发现 ${this.#localCandidateCount} 条本地路径，正在与主机配对`);
      const signal: LocalIceSignal = {
        type: "webrtc_ice", session_id: this.#sessionId, candidate: candidate.candidate,
        sdp_mid: candidate.sdpMid, sdp_mline_index: candidate.sdpMLineIndex, username_fragment: candidate.usernameFragment,
      };
      // Chromium can emit host candidates synchronously while
      // setLocalDescription() is resolving. Sending one before the offer makes
      // the Host discard it because that session does not exist yet. Preserve
      // WebSocket ordering explicitly; Safari generally gathers slowly enough
      // that it did not expose this race.
      if (this.#offerSent) this.#signaling.send(signal);
      else this.#pendingLocalIce.push(signal);
    };
    peer.onicecandidateerror = (event) => {
      const detail = `ICE 服务器 ${event.url || "未知地址"} 返回 ${event.errorCode}：${event.errorText || "连接失败"}`;
      this.#progress("candidates", detail);
      void this.#report(`ice-server-error:${event.errorCode}:${event.errorText || "unknown"}:${event.url || "unknown"}`);
    };
    peer.onconnectionstatechange = () => {
      void this.#report(`peer:${peer.connectionState}`);
      if (peer.connectionState === "connected") {
        this.#setState("connected");
        this.#progress("video");
        this.#stats = new StatsMonitor(peer, this.statsOutput);
        this.#stats.start();
        for (const delay of [0, 2000, 5000, 15000]) {
          this.#reportTimers.push(window.setTimeout(() => { void this.#report(`connected:${delay}`); }, delay));
        }
      } else if (peer.connectionState === "connecting") {
        this.#progress("secure", "候选路径正在进行加密握手与连通性验证");
      } else if (peer.connectionState === "disconnected") {
        this.#setState("reconnecting");
        this.#progress("reconnecting");
      }
      else if (!this.#closing && (peer.connectionState === "failed" || peer.connectionState === "closed")) {
        this.#progress("failed", "所有网络候选均未通过连通性检查");
        this.#disconnect("远程桌面连接已中断，请检查网络或主机状态。");
      }
    };
    peer.oniceconnectionstatechange = () => {
      void this.#report(`ice:${peer.iceConnectionState}`);
      if (peer.iceConnectionState === "checking") {
        this.#progress("candidates", `正在验证 ${this.#localCandidateCount} 条本地路径与 ${this.#remoteCandidateCount} 条主机路径`);
      } else if (peer.iceConnectionState === "connected" || peer.iceConnectionState === "completed") {
        this.#progress("secure");
      } else if (peer.iceConnectionState === "disconnected") {
        this.#progress("reconnecting");
      } else if (peer.iceConnectionState === "failed") {
        this.#progress("failed", "ICE 检查失败：浏览器与主机之间没有可用网络路径");
      }
    };
    peer.onicegatheringstatechange = () => {
      void this.#report(`gathering:${peer.iceGatheringState}`);
      if (peer.iceGatheringState === "complete") {
        this.#progress("candidates", `候选收集完成，共发现 ${this.#localCandidateCount} 条本地路径`);
        if (this.#localCandidateCount === 0) {
          candidateFailed("Chrome 未能生成任何网络候选；旧连接已保留，请检查浏览器网络权限");
        }
      }
    };
    const offer = await peer.createOffer();
    this.#progress("candidates", "正在预检新连接路径；确认可用后才会接管旧连接");
    await peer.setLocalDescription(offer);
    const candidateTimeout = window.setTimeout(() => {
      candidateFailed("Chrome 在 10 秒内未能生成网络候选；旧连接已保留");
    }, 10_000);
    try {
      await firstCandidate;
    } catch (error) {
      this.close();
      throw error;
    } finally {
      window.clearTimeout(candidateTimeout);
    }

    // Only now create the authoritative session. This is the atomic takeover
    // point: the signaling server evicts every older device session, but a
    // broken browser can no longer destroy a healthy Safari connection before
    // it has proved that it owns at least one usable ICE path.
    this.#progress("session", "已找到可用局域网路径，正在接管旧连接");
    const { session_id, session_token } = await createSession(deviceId);
    this.#sessionId = session_id;
    this.#sessionToken = session_token;
    this.#setState("negotiating");
    for (const delay of [1000, 3000, 8000]) {
      this.#reportTimers.push(window.setTimeout(() => {
        if (this.state === "negotiating") void this.#report(`negotiating:${delay}`);
      }, delay));
    }
    const viewport = this.#physicalVideoArea();
    this.#signaling.send({
      type: "webrtc_offer", session_id: this.#sessionId, session_token: this.#sessionToken,
      // Send the candidate-free SDP returned by createOffer(), then trickle the
      // queued candidates below. Chrome's localDescription already contains
      // its original `.local` candidate at this point. Embedding that candidate
      // and then sending the server-rewritten LAN-IP form with the same
      // foundation lets the Host deduplicate the usable form and leaves macOS
      // Chrome checking an unresolvable route. The original working Chrome
      // implementation used this candidate-free offer ordering.
      sdp: offer.sdp ?? "", viewport_width: viewport.width, viewport_height: viewport.height,
    });
    this.#offerSent = true;
    for (const candidate of this.#pendingLocalIce.splice(0)) {
      candidate.session_id = this.#sessionId;
      this.#signaling.send(candidate);
    }
    this.#progress("candidates", `已向主机发送连接请求，正在等待网络候选（${this.#localCandidateCount}）`);
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

  pasteHostClipboard(): Promise<string> {
    return this.#clipboardRequest({ type: "paste" });
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
    if (this.state === "closed" || this.#closing) return;
    this.#closing = true;
    if (this.#sessionId) {
      try { this.#signaling.send({ type: "session_close", session_id: this.#sessionId }); } catch { /* already disconnected */ }
    }
    this.#input?.destroy();
    window.clearTimeout(this.#clipboardPullTimer);
    this.#clipboard?.removeEventListener("message", this.#onClipboardMessage);
    this.#clipboard = null;
    this.#pendingLocalIce.length = 0;
    for (const name of ["loadedmetadata", "canplay", "playing", "waiting", "stalled", "error"] as const) {
      this.video.removeEventListener(name, this.#onMediaEvent);
    }
    for (const request of this.#clipboardRequests.values()) {
      window.clearTimeout(request.timer);
      request.reject(new Error("远程会话已关闭"));
    }
    this.#clipboardRequests.clear();
    this.#stats?.stop();
    for (const timer of this.#reportTimers.splice(0)) window.clearTimeout(timer);
    this.#peer?.close();
    this.#signaling.removeEventListener("close", this.#onSignalingClose);
    this.#signaling.close();
    this.video.srcObject = null;
    this.#setState("closed");
  }

  #onSignal = (event: Event): void => {
    void this.#handleSignal((event as CustomEvent<ServerSignal>).detail).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      void this.#report(`signal-error:${message}`);
      this.dispatchEvent(new CustomEvent("error", { detail: message }));
    });
  };

  #onSignalingClose = (): void => {
    this.#progress("failed", "实时信令通道已关闭");
    this.#disconnect("Web 连接已断开，请检查网络后重新连接。");
  };

  #onMediaEvent = (event: Event): void => {
    void this.#report(`media:${event.type}`);
    if (event.type === "loadedmetadata") {
      this.#progress("video", "视频轨道已到达，正在解码首个关键帧");
    } else if (event.type === "playing") {
      this.#progress("ready");
    } else if ((event.type === "waiting" || event.type === "stalled") && this.state === "connected") {
      this.#progress("video", "安全通道已连接，正在等待后续视频帧");
    } else if (event.type === "error") {
      this.#progress("failed", this.video.error?.message ?? "浏览器无法播放远程视频");
    }
  };

  #disconnect(message: string): void {
    if (this.#closing || this.state === "closed" || this.#disconnectReported) return;
    this.#disconnectReported = true;
    this.dispatchEvent(new CustomEvent<SessionDisconnectDetail>("disconnect", { detail: { message } }));
    this.close();
  }

  #clipboardRequest(message: { type: "read" } | { type: "write"; text: string; paste: boolean } | { type: "paste" }): Promise<string> {
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
      this.#progress("candidates", "主机已应答，正在验证双方网络路径");
      this.#requestLowLatencyPlayback();
      for (const candidate of this.#pendingIce.splice(0)) await this.#addRemoteIce(candidate);
    }
    if (signal.type === "webrtc_ice") {
      this.#remoteCandidateCount += 1;
      const candidate = remoteIceCandidate(signal);
      this.#progress("candidates", `已收到 ${this.#remoteCandidateCount} 条主机路径，正在逐一验证`);
      if (this.#peer?.remoteDescription) await this.#addRemoteIce(candidate);
      else this.#pendingIce.push(candidate);
    }
    if (signal.type === "session_closed") {
      this.#disconnect(disconnectMessage(signal.reason));
    }
    if (signal.type === "error") this.dispatchEvent(new CustomEvent("error", { detail: signal.message }));
  }

  #setState(state: SessionState): void {
    this.state = state;
    this.dispatchEvent(new CustomEvent<SessionState>("state", { detail: state }));
  }

  #progress(phase: ConnectionPhase, detail?: string): void {
    this.dispatchEvent(new CustomEvent<ConnectionProgressDetail>("progress", {
      detail: { phase, detail },
    }));
  }

  async #addRemoteIce(candidate: RTCIceCandidateInit): Promise<void> {
    try {
      await this.#peer?.addIceCandidate(candidate);
    } catch (error) {
      // A stale or interface-specific candidate must not prevent Chrome from
      // trying every remaining candidate in this BUNDLE transport.
      const message = error instanceof Error ? error.message : String(error);
      void this.#report(`ice-candidate-error:${message}`);
    }
  }

  #requestLowLatencyPlayback(): void {
    // One to two frames are enough to absorb LAN/capture scheduling variance.
    // A zero-sized buffer exposes every small arrival-time fluctuation as a
    // repeated or skipped frame, which is especially visible on 60 FPS video.
    const jitterBufferMs = 35;
    const playoutDelaySeconds = jitterBufferMs / 1000;
    type LowLatencyReceiver = RTCRtpReceiver & {
      jitterBufferTarget?: number;
      playoutDelayHint?: number;
    };
    for (const receiver of this.#peer?.getReceivers() ?? []) {
      if (receiver.track?.kind !== "video") continue;
      const lowLatency = receiver as LowLatencyReceiver;
      try { lowLatency.jitterBufferTarget = jitterBufferMs; } catch { /* browser-controlled fallback */ }
      try { lowLatency.playoutDelayHint = playoutDelaySeconds; } catch { /* older browser fallback */ }
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
    const reports = this.#peer ? [...(await this.#peer.getStats()).values()] : [];
    const inbound = reports.find((item) => item.type === "inbound-rtp" && item.kind === "video");
    const audioInbound = reports.find((item) => item.type === "inbound-rtp" && item.kind === "audio");
    const candidates = new Map(reports
      .filter((item) => item.type === "local-candidate" || item.type === "remote-candidate")
      .map((item) => [item.id, item]));
    const candidatePairs = reports.filter((item) => item.type === "candidate-pair").slice(0, 16).map((pair) => {
      const local = candidates.get(pair.localCandidateId);
      const remote = candidates.get(pair.remoteCandidateId);
      return {
        state: pair.state ?? null,
        nominated: pair.nominated ?? false,
        selected: pair.selected ?? false,
        requestsSent: pair.requestsSent ?? 0,
        responsesReceived: pair.responsesReceived ?? 0,
        requestsReceived: pair.requestsReceived ?? 0,
        responsesSent: pair.responsesSent ?? 0,
        local: local ? { type: local.candidateType, protocol: local.protocol, relayProtocol: local.relayProtocol ?? null } : null,
        remote: remote ? { type: remote.candidateType, protocol: remote.protocol, relayProtocol: remote.relayProtocol ?? null } : null,
      };
    });
    const quality = this.video.getVideoPlaybackQuality?.();
    await reportClient({
      sessionId: this.#sessionId || null,
      stage,
      iceTransportPolicy: this.#peer?.getConfiguration().iceTransportPolicy ?? null,
      iceServerUrls: (this.#peer?.getConfiguration().iceServers ?? []).flatMap((server) => (
        typeof server.urls === "string" ? [server.urls] : server.urls
      )),
      connectionState: this.#peer?.connectionState ?? null,
      iceConnectionState: this.#peer?.iceConnectionState ?? null,
      localCandidateCount: this.#localCandidateCount,
      remoteCandidateCount: this.#remoteCandidateCount,
      candidatePairs,
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
