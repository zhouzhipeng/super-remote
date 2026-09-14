export class StatsMonitor {
  #timer = 0;
  #lastBytes = 0;
  #lastAudioBytes = 0;
  #lastTime = 0;
  #inputLatencyMs: number | null = null;
  #lastTileBytes = 0;

  constructor(private readonly peer: RTCPeerConnection, private readonly output: HTMLElement,
    private readonly video?: HTMLVideoElement) {}

  start(): void {
    this.#timer = window.setInterval(() => void this.#refresh(), 1000);
    void this.#refresh();
  }

  stop(): void { clearInterval(this.#timer); }

  setInputLatency(milliseconds: number): void { this.#inputLatencyMs = milliseconds; }

  async #refresh(): Promise<void> {
    const reports = await this.peer.getStats();
    let fps = 0, bytes = 0, audioBytes = 0, packetsLost = 0, packetsReceived = 0, jitterMs = 0, rttMs = 0;
    let codec = "—", route = "connecting";
    const codecs = new Map<string, string>();
    reports.forEach((report) => {
      if (report.type === "codec") codecs.set(report.id, report.mimeType ?? "—");
      if (report.type === "inbound-rtp" && report.kind === "video") {
        fps = report.framesPerSecond ?? 0;
        bytes = report.bytesReceived ?? 0;
        packetsLost = report.packetsLost ?? 0;
        packetsReceived = report.packetsReceived ?? 0;
        jitterMs = (report.jitter ?? 0) * 1000;
        codec = codecs.get(report.codecId) ?? codec;
      }
      if (report.type === "inbound-rtp" && report.kind === "audio") {
        audioBytes = report.bytesReceived ?? 0;
      }
      if (report.type === "candidate-pair" && report.nominated && report.state === "succeeded") {
        rttMs = (report.currentRoundTripTime ?? 0) * 1000;
        const remote = reports.get(report.remoteCandidateId);
        route = remote?.candidateType === "relay" ? "TURN" : "P2P";
      }
    });
    const now = performance.now();
    const seconds = this.#lastTime ? (now - this.#lastTime) / 1000 : 0;
    const tiles = this.video?.dataset.displayTransport === "lossless-tiles";
    const tileBytes = Number(this.video?.dataset.tileBytes ?? 0);
    const tileMbps = seconds ? Math.max(0, tileBytes - this.#lastTileBytes) * 8 / seconds / 1_000_000 : 0;
    this.#lastTileBytes = tileBytes;
    const bitrate = this.#lastTime ? ((bytes - this.#lastBytes) * 8) / ((now - this.#lastTime) / 1000) / 1_000_000 : 0;
    const audioKbps = this.#lastTime ? ((audioBytes - this.#lastAudioBytes) * 8) / ((now - this.#lastTime) / 1000) / 1_000 : 0;
    this.#lastBytes = bytes;
    this.#lastAudioBytes = audioBytes;
    this.#lastTime = now;
    const loss = packetsLost + packetsReceived > 0 ? (packetsLost / (packetsLost + packetsReceived)) * 100 : 0;
    const inputLatency = this.#inputLatencyMs === null ? "—" : this.#inputLatencyMs.toFixed(1);
    if (tiles) {
      this.output.textContent = `静止无损补偿 · 视频 ${fps.toFixed(0)} FPS | ${(bitrate + tileMbps).toFixed(1)} Mbps | 更新 ${this.video?.dataset.tileCount ?? 0} 块 · 复用 ${this.video?.dataset.tileCopies ?? 0} 块 | 绘制 ${this.video?.dataset.tileDecodeMs ?? "—"} ms | Input RTT ${inputLatency} ms | ${route}`;
      return;
    }
    this.output.textContent = `FPS ${fps.toFixed(0)}  |  ${bitrate.toFixed(1)} Mbps  |  音频 ${audioKbps.toFixed(0)} kbps  |  RTT ${rttMs.toFixed(0)} ms  |  Input RTT ${inputLatency} ms  |  Loss ${loss.toFixed(1)}%  |  Jitter ${jitterMs.toFixed(1)} ms  |  ${codec.replace("video/", "")}  |  ${route}`;
  }
}
