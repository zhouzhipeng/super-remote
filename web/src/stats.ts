export class StatsMonitor {
  #timer = 0;
  #lastBytes = 0;
  #lastAudioBytes = 0;
  #lastTime = 0;

  constructor(private readonly peer: RTCPeerConnection, private readonly output: HTMLElement) {}

  start(): void {
    this.#timer = window.setInterval(() => void this.#refresh(), 1000);
    void this.#refresh();
  }

  stop(): void { clearInterval(this.#timer); }

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
    const bitrate = this.#lastTime ? ((bytes - this.#lastBytes) * 8) / ((now - this.#lastTime) / 1000) / 1_000_000 : 0;
    const audioKbps = this.#lastTime ? ((audioBytes - this.#lastAudioBytes) * 8) / ((now - this.#lastTime) / 1000) / 1_000 : 0;
    this.#lastBytes = bytes;
    this.#lastAudioBytes = audioBytes;
    this.#lastTime = now;
    const loss = packetsLost + packetsReceived > 0 ? (packetsLost / (packetsLost + packetsReceived)) * 100 : 0;
    this.output.textContent = `FPS ${fps.toFixed(0)}  |  ${bitrate.toFixed(1)} Mbps  |  音频 ${audioKbps.toFixed(0)} kbps  |  RTT ${rttMs.toFixed(0)} ms  |  Loss ${loss.toFixed(1)}%  |  Jitter ${jitterMs.toFixed(1)} ms  |  ${codec.replace("video/", "")}  |  ${route}`;
  }
}
