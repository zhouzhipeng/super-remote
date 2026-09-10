// Live opt-in benchmark: real post-SendInput ACK, zero-distance mouse events
// on BOTH input channels. Does not type, click, move the pointer, or save pixels.
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
assert.equal(process.env.REMOTE_LIVE_TEST, "1");
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const status = JSON.parse(fs.readFileSync(process.env.REMOTE_STATUS_PATH, "utf8"));
const browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
try {
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 });
  await context.addInitScript((forceRelay) => {
    const NativeSocket = window.WebSocket;
    window.WebSocket = class extends NativeSocket {
      constructor(url, protocols) {
        super(url, protocols);
        if (String(url).includes("/api/ws?")) {
          window.__controlSocket = this;
          this.addEventListener("message", event => {
            const signal = JSON.parse(event.data);
            if (signal.type === "webrtc_answer" && signal.input_control) window.__controlSession = signal.session_id;
          });
        }
      }
    };
    const Native = window.RTCPeerConnection;
    window.__inputChannels = {};
    window.RTCPeerConnection = class extends Native {
      constructor(config) {
        if (forceRelay) config = { ...config, iceTransportPolicy: "relay",
          iceServers: config.iceServers.map(server => ({ ...server,
            urls: [server.urls].flat().filter(url => url === `turn:${location.host}?transport=tcp`),
          })).filter(server => server.urls.length) };
        super(config); window.__inputPeer = this;
      }
      createDataChannel(label, options) {
        const channel = super.createDataChannel(label, options);
        window.__inputChannels[label] = channel;
        return channel;
      }
    };
  }, process.env.REMOTE_FORCE_TCP_RELAY === "1");
  const page = await context.newPage();
  const access = new URL(status.direct_url);
  if (process.env.REMOTE_ACCESS_ORIGIN) {
    const origin = new URL(process.env.REMOTE_ACCESS_ORIGIN);
    access.protocol = origin.protocol; access.host = origin.host;
  }
  await page.goto(access.href).catch(() => { throw new Error("Live endpoint unavailable (authenticated URL omitted)"); });
  await page.waitForFunction(() => window.__inputChannels["input-reliable"]?.readyState === "open"
    && (document.querySelector("video")?.currentTime > 2 || window.__controlSession), null, { timeout: 60_000 });
  await page.waitForTimeout(10_000); // exercise control while the real encoder runs
  const result = await page.evaluate(async () => {
    const output = {};
    for (const label of ["input-fast", "input-reliable"]) {
      let channel = window.__inputChannels[label];
      let controlListener;
      if (document.querySelector("video")?.dataset.inputTransport === "websocket-control" && window.__controlSession) {
        channel = new EventTarget();
        channel.send = packet => window.__controlSocket.send(JSON.stringify({ type: "input_packet",
          session_id: window.__controlSession, data: [...new Uint8Array(packet)] }));
        controlListener = event => {
          const signal = JSON.parse(event.data);
          if (signal.type === "input_ack" && signal.session_id === window.__controlSession)
            channel.dispatchEvent(new MessageEvent("message", { data: Uint8Array.from(signal.data).buffer }));
        };
        window.__controlSocket.addEventListener("message", controlListener);
      }
      const times = [];
      let lost = 0;
      for (let i = 0; i < 120; i++) {
        const packet = new ArrayBuffer(16);
        const view = new DataView(packet);
        view.setUint8(0, 5); // relative move dx=dy=0
        view.setUint8(1, 1); // ACK after actual Windows injection
        view.setUint16(2, 4, true);
        const token = BigInt(Math.round((performance.timeOrigin + performance.now()) * 1000));
        view.setBigUint64(4, token, true);
        const elapsed = await new Promise(resolve => {
          const started = performance.now();
          const listener = event => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 12
              || new DataView(event.data).getBigUint64(4, true) !== token) return;
            clearTimeout(timer); channel.removeEventListener("message", listener);
            resolve(performance.now() - started);
          };
          const timer = setTimeout(() => { channel.removeEventListener("message", listener); resolve(null); }, 1000);
          channel.addEventListener("message", listener);
          channel.send(packet);
        });
        if (elapsed === null) lost++; else times.push(elapsed);
        if (lost >= 5) break; // fail fast instead of silently waiting four minutes
        await new Promise(resolve => setTimeout(resolve, 8));
      }
      times.sort((a, b) => a - b);
      output[label] = { samples: times.length, lost, medianMs: times[Math.floor(times.length * .5)],
        p95Ms: times[Math.floor(times.length * .95)], maxMs: times.at(-1) };
      if (controlListener) window.__controlSocket.removeEventListener("message", controlListener);
    }
    return output;
  });
  console.log(JSON.stringify(result, null, 2));
  console.log("Route", await page.evaluate(async () => {
    const reports = [...(await window.__inputPeer.getStats()).values()];
    const transport = reports.find(item => item.type === "transport" && item.selectedCandidatePairId);
    const pair = reports.find(item => item.id === transport?.selectedCandidatePairId);
    const local = reports.find(item => item.id === pair?.localCandidateId);
    return { candidateType: local?.candidateType, relayProtocol: local?.relayProtocol,
      inputTransport: document.querySelector("video")?.dataset.inputTransport,
      rttMs: (pair?.currentRoundTripTime ?? 0) * 1000 };
  }));
  await page.locator("#back").click();
  for (const value of Object.values(result)) assert.ok(value.samples >= 110, "input ACK loss");
} finally { await browser.close(); }
