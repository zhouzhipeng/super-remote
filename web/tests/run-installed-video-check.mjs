// Explicit opt-in live test: connects to the installed Host, takes ownership of
// its remote session, and measures received frames. No input events or screenshots.
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
assert.equal(process.env.REMOTE_LIVE_TEST, "1", "Set REMOTE_LIVE_TEST=1 only after authorizing a real session takeover");
const status = JSON.parse(fs.readFileSync(process.env.REMOTE_STATUS_PATH, "utf8"));
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
try {
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2,
    ...(process.env.TEST_SAFARI_ROUTING === "1" ? { userAgent: "Mozilla/5.0 Version/18.5 Safari/605.1.15" } : {}) });
  await context.addInitScript((forceRelay) => {
    const NativePeer = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends NativePeer {
      constructor(config) {
        if (forceRelay) config = { ...config, iceTransportPolicy: "relay",
          iceServers: config.iceServers.map(server => ({ ...server,
            urls: [server.urls].flat().filter(url => url === `turn:${location.host}?transport=tcp`),
          })).filter(server => server.urls.length) };
        super(config);
      }
      addTransceiver(kind, init) {
        if (kind === "video") window.__videoCheck = this;
        return super.addTransceiver(kind, init);
      }
    };
  }, process.env.REMOTE_FORCE_TCP_RELAY === "1");
  const page = await context.newPage();
  const url = new URL(status.direct_url);
  if (process.env.REMOTE_ACCESS_ORIGIN) {
    const origin = new URL(process.env.REMOTE_ACCESS_ORIGIN);
    url.protocol = origin.protocol; url.host = origin.host;
  }
  // The token stays local and is never printed or saved by the test.
  await page.goto(url.href).catch(() => { throw new Error("Live endpoint unavailable (authenticated URL omitted)"); });
  await page.waitForFunction(() => {
    const video = document.querySelector("video");
    return window.__videoCheck?.connectionState === "connected" && video?.currentTime > 2;
  }, null, { timeout: 60_000 });
  const sample = () => page.evaluate(async () => {
    const reports = [...(await window.__videoCheck.getStats()).values()];
    const inbound = reports.find((item) => item.type === "inbound-rtp" && item.kind === "video");
    const codec = reports.find((item) => item.id === inbound.codecId);
    const video = document.querySelector("video");
    return { time: performance.now(), frames: inbound.framesDecoded, width: video.videoWidth,
      height: video.videoHeight, codec: codec.mimeType, freezeCount: inbound.freezeCount ?? 0,
      connection: window.__videoCheck.connectionState,
      packetsLost: inbound.packetsLost ?? 0, framesDropped: inbound.framesDropped ?? 0,
      bytesReceived: inbound.bytesReceived ?? 0,
      totalDecodeTime: inbound.totalDecodeTime ?? 0 };
  });
  const first = await sample();
  await page.waitForTimeout(30_000);
  const last = await sample();
  const result = { ...last, sampledFrames: last.frames - first.frames,
    sampleSeconds: (last.time - first.time) / 1000,
    measuredFps: (last.frames - first.frames) * 1000 / (last.time - first.time),
    newFreezes: last.freezeCount - first.freezeCount };
  delete result.time;
  console.log(JSON.stringify(result, null, 2));
  assert.equal(last.connection, "connected");
  assert.equal(last.codec, "video/H264");
  assert.ok(result.measuredFps >= 55, "real received video did not sustain approximately 60 FPS");
  assert.equal(result.newFreezes, 0);
  // Use the app's normal disconnect to stop capture immediately.
  await page.locator("#back").click();
} finally { await browser.close(); }
