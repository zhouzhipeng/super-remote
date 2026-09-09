// Isolated browser regression: the real Web UI + RemoteSession receive a local
// synthetic WebRTC desktop. No installed service, credentials or user session.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const server = await createServer({
  root: fileURLToPath(new URL("..", import.meta.url)),
  server: { host: "127.0.0.1", port: 0 },
});
await server.listen();
let browser;
try {
  browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
  const context = await browser.newContext({ viewport: { width: 1280, height: 880 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => { errors.push(error.message); console.error("Browser error:", error.message); });
  await page.route("**/api/**", (route) => {
    const path = new URL(route.request().url()).pathname;
    const value = path.endsWith("ws-ticket") ? { ticket: "local-test" }
      : path.endsWith("sessions") ? { session_id: "pixel-test", session_token: "local-test" }
      : {};
    return route.fulfill({ status: path.endsWith("turn-credentials") ? 404 : 200,
      contentType: "application/json", body: JSON.stringify(value) });
  });
  await page.addInitScript(() => {
    const NativePeer = window.RTCPeerConnection;
    const NativeWebSocket = window.WebSocket;
    window.__test = { offers: [], assignments: 0, overlayFlashes: 0 };
    const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "srcObject");
    Object.defineProperty(HTMLMediaElement.prototype, "srcObject", {
      ...descriptor, set(value) { window.__test.assignments++; descriptor.set.call(this, value); },
    });
    window.RTCPeerConnection = class extends NativePeer {
      constructor(configuration) {
        super({ ...configuration, iceServers: [] });
        window.__test.client = this;
      }
    };
    window.WebSocket = class extends EventTarget {
      static OPEN = 1;
      readyState = 1;
      constructor(url, protocols) {
        super();
        // Vite's hot-reload socket must keep its real implementation.
        if (!String(url).includes("/api/ws?")) return new NativeWebSocket(url, protocols);
        setTimeout(() => this.onopen?.(new Event("open")), 0);
      }
      close() { this.readyState = 3; this.onclose?.(new Event("close")); }
      send(data) { void this.handle(JSON.parse(data)); }
      async handle(signal) {
        if (signal.type === "webrtc_offer") {
          window.__test.offers.push(signal);
          const peer = new NativePeer({ iceServers: [] });
          this.peer = peer;
          const send = (message) => this.onmessage?.({ data: JSON.stringify({ session_id: "pixel-test", ...message }) });
          peer.onicecandidate = ({ candidate }) => {
            if (candidate) send({ type: "webrtc_ice", candidate: candidate.candidate,
              sdp_mid: candidate.sdpMid, sdp_mline_index: candidate.sdpMLineIndex });
          };
          const canvas = document.createElement("canvas");
          canvas.width = 2560; canvas.height = 1600;
          const ctx = canvas.getContext("2d");
          const draw = () => {
            ctx.fillStyle = "#20242c"; ctx.fillRect(0, 0, 2560, 1600);
            ctx.fillStyle = "#f1f3f5"; ctx.font = "32px monospace";
            ctx.fillText("Retina stable desktop 2560 x 1600", 60, 120);
            requestAnimationFrame(draw);
          };
          draw();
          const stream = canvas.captureStream(60);
          const track = stream.getVideoTracks()[0];
          track.contentHint = "detail";
          const sender = peer.addTrack(track, stream);
          await peer.setRemoteDescription({ type: "offer", sdp: signal.sdp });
          for (const candidate of this.pending ?? []) await peer.addIceCandidate(candidate);
          this.pending = [];
          await peer.setLocalDescription(await peer.createAnswer());
          const parameters = sender.getParameters();
          parameters.degradationPreference = "maintain-resolution";
          for (const encoding of parameters.encodings) encoding.maxBitrate = 20_000_000;
          await sender.setParameters(parameters);
          send({ type: "webrtc_answer", sdp: peer.localDescription.sdp });
        } else if (signal.type === "webrtc_ice") {
          const candidate = { candidate: signal.candidate, sdpMid: signal.sdp_mid, sdpMLineIndex: signal.sdp_mline_index };
          if (this.peer?.remoteDescription) await this.peer.addIceCandidate(candidate);
          else (this.pending ??= []).push(candidate);
        } else if (signal.type === "session_close") this.peer?.close();
      }
    };
  });
  await page.goto(`${server.resolvedUrls.local[0]}#token=local-test&device=synthetic`);
  try { await page.waitForFunction(() => {
    const video = document.querySelector("video");
    return video?.videoWidth > 0 && video.currentTime > 0.2 && document.querySelector(".connection-overlay").hidden;
  }, null, { timeout: 20_000 }); } catch (error) {
    console.error(await page.evaluate(() => ({ state: document.querySelector("#state")?.textContent,
      description: document.querySelector("#connection-description")?.textContent,
      width: document.querySelector("video")?.videoWidth, time: document.querySelector("video")?.currentTime,
      peer: window.__test.client?.connectionState, offers: window.__test.offers.length })));
    throw error;
  }
  const result = await page.evaluate(async () => {
    const test = window.__test;
    const video = document.querySelector("video");
    const overlay = document.querySelector(".connection-overlay");
    const rect = video.getBoundingClientRect();
    const source = video.srcObject;
    const beforeAssignments = test.assignments;
    const observer = new MutationObserver(() => {
      if (!overlay.hidden && !overlay.classList.contains("is-complete")) test.overlayFlashes++;
    });
    observer.observe(overlay, { attributes: true });
    for (let i = 0; i < 12; i++) {
      video.dispatchEvent(new Event("waiting"));
      await new Promise((resolve) => setTimeout(resolve, 10));
      video.dispatchEvent(new Event("playing"));
      video.dispatchEvent(new Event("stalled"));
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    // Late ICE diagnostics previously reopened the initial loading overlay too.
    test.client.onicecandidateerror({ errorCode: 701, errorText: "synthetic late candidate", url: "local-test" });
    video.dispatchEvent(new Event("playing"));
    const audio = new AudioContext();
    const destination = audio.createMediaStreamDestination();
    const audioTrack = destination.stream.getAudioTracks()[0];
    test.client.ontrack({ track: audioTrack });
    await new Promise((resolve) => setTimeout(resolve, 1100));
    observer.disconnect();
    const reports = [...(await test.client.getStats()).values()];
    const inbound = reports.find((r) => r.type === "inbound-rtp" && r.kind === "video");
    const result = { overlayFlashes: test.overlayFlashes, overlayHidden: overlay.hidden,
      sourcePreserved: source === video.srcObject, lateTrackAssignments: test.assignments - beforeAssignments,
      sessions: test.offers.length, viewport: test.offers[0],
      expectedPixels: { width: Math.floor(rect.width * 2), height: Math.floor(rect.height * 2) },
      videoWidth: video.videoWidth, videoHeight: video.videoHeight, decodedFrames: inbound?.framesDecoded };
    audioTrack.stop(); await audio.close();
    return result;
  });
  delete result.viewport.session_token;
  delete result.viewport.sdp;
  console.log(JSON.stringify(result, null, 2));
  assert.deepEqual(errors, [], "unexpected browser errors");
  assert.equal(result.overlayFlashes, 0, "playback events flashed the loading overlay");
  assert.equal(result.overlayHidden, true);
  assert.equal(result.sourcePreserved, true);
  assert.equal(result.lateTrackAssignments, 0, "late audio reset the video source");
  assert.equal(result.sessions, 1, "playback/layout changes reconnected the session");
  assert.equal(result.viewport.viewport_width, result.expectedPixels.width);
  assert.equal(result.viewport.viewport_height, result.expectedPixels.height);
  assert.ok(result.decodedFrames > 30, "real WebRTC video was not decoded");
} finally {
  await browser?.close();
  await server.close();
}
