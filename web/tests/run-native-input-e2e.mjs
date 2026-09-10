// Isolated real Rust Host + signaling + browser. Video is synthetic; input
// probes inject only a zero-distance relative move, never clicks or text.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import net from "node:net";
import { once } from "node:events";
import { randomBytes } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../..", import.meta.url));
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
assert.ok(process.env.FFMPEG_EXECUTABLE, "FFMPEG_EXECUTABLE is required for synthetic video");
const temp = fs.mkdtempSync(path.join(os.tmpdir(), "remote-dual-input-"));
const children = [];
const logs = [];
let browser;
let proxy;
const proxySockets = new Set();
function start(executable, args, env = {}) {
  const child = spawn(executable, args, { cwd: root, windowsHide: true, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  children.push(child);
  child.on("error", error => logs.push(error.message));
  for (const pipe of [child.stdout, child.stderr]) pipe.on("data", data => logs.push(data.toString()));
  return child;
}
try {
  const clip = path.join(temp, "synthetic.h264");
  const generated = spawnSync(process.env.FFMPEG_EXECUTABLE, ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
    "testsrc2=size=640x360:rate=30", "-t", "40", "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-profile:v", "baseline", "-g", "30", "-f", "h264", clip], { windowsHide: true, timeout: 30_000 });
  assert.equal(generated.status, 0, "synthetic video generation failed");
  const probe = net.createServer(); probe.listen(0, "127.0.0.1"); await once(probe, "listening");
  const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
  const origin = `http://127.0.0.1:${port}`;
  const password = randomBytes(24).toString("hex"), token = randomBytes(24).toString("hex");
  start(process.env.SIGNALING_EXECUTABLE || path.join(root, "target/debug/remote-signaling.exe"), [], {
    REMOTE_BIND: `127.0.0.1:${port}`, REMOTE_ADMIN_USER: "input-test", REMOTE_ADMIN_PASSWORD: password,
    REMOTE_JWT_SECRET: randomBytes(32).toString("hex"), REMOTE_DEVICE_TOKEN: token,
    REMOTE_TURN_URLS: "", RUST_LOG: "remote_signaling=warn",
  });
  let ready = false;
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch(`${origin}/api/healthz`)).ok) { ready = true; break; } } catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.ok(ready, "isolated signaling failed to start");
  const config = path.join(temp, "host.toml");
  fs.writeFileSync(config, `server_url = ${JSON.stringify(origin)}\ndevice_id = "native-input-test"\ndevice_name = "Native Input Test"\ndevice_token = "${token}"\nwidth = 640\nheight = 360\nfps = 30\nbitrate = 2000000\nh264_file = ${JSON.stringify(clip)}\n`);
  start(process.env.HOST_EXECUTABLE || path.join(root, "target/debug/remote-host.exe"), [config]);
  browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
  const page = await browser.newPage({ viewport: { width: 1000, height: 750 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    const Native = RTCPeerConnection;
    window.__dual = { channels: {} };
    const NativeSocket = WebSocket;
    window.WebSocket = class extends NativeSocket {
      constructor(url, protocols) {
        super(url, protocols);
        this.addEventListener("message", ({ data }) => {
          const message = JSON.parse(data);
          if (message.type === "webrtc_answer") { window.__dual.socket = this; window.__dual.session = message.session_id; }
        });
      }
    };
    window.RTCPeerConnection = class extends Native {
      addTransceiver(kind, init) { if (kind === "video") window.__dual.video = this; return super.addTransceiver(kind, init); }
      createDataChannel(label, options) {
        if (label === "input-fast") window.__dual.input = this;
        const channel = super.createDataChannel(label, options); window.__dual.channels[label] = channel; return channel;
      }
    };
  });
  // Emulate an FRP/reverse-proxy idle timeout. RTC input and media do not
  // refresh the separate signaling socket; server Ping must keep it alive.
  proxy = net.createServer(client => {
    const upstream = net.connect(port, "127.0.0.1");
    for (const socket of [client, upstream]) {
      proxySockets.add(socket);
      socket.setTimeout(20_000, () => socket.destroy());
      socket.on("error", () => { client.destroy(); upstream.destroy(); });
      socket.on("close", () => { proxySockets.delete(socket); client.destroy(); upstream.destroy(); });
    }
    client.pipe(upstream).pipe(client);
  });
  proxy.listen(0, "127.0.0.1"); await once(proxy, "listening");
  await page.goto(`http://127.0.0.1:${proxy.address().port}`);
  await page.locator('[name="username"]').fill("input-test");
  await page.locator('[name="password"]').fill(password);
  await page.locator('form button').click();
  await page.locator('[data-device="native-input-test"]').click();
  await page.waitForFunction(() => document.querySelector("video")?.currentTime > 1
    && document.querySelector("video").dataset.inputTransport === "webrtc-input", null, { timeout: 30_000 });
  const result = await page.evaluate(async () => {
    const route = async peer => {
      const stats = await peer.getStats();
      const transport = [...stats.values()].find(v => v.type === "transport" && v.selectedCandidatePairId);
      const pair = stats.get(transport.selectedCandidatePairId);
      const local = stats.get(pair.localCandidateId), remote = stats.get(pair.remoteCandidateId);
      return { localPort: local.port, remotePort: remote.port, protocol: local.protocol };
    };
    const latency = {};
    for (const label of ["input-fast", "input-reliable"]) {
      const channel = window.__dual.channels[label], samples = [];
      for (let i = 0; i < 40; i++) {
        const data = new ArrayBuffer(16), view = new DataView(data);
        view.setUint8(0, 5); view.setUint8(1, 1); view.setUint16(2, 4, true);
        const token = BigInt(Math.round((performance.timeOrigin + performance.now()) * 1000));
        view.setBigUint64(4, token, true);
        samples.push(await new Promise((resolve, reject) => {
          const start = performance.now();
          const listener = event => {
            if (!(event.data instanceof ArrayBuffer) || new DataView(event.data).getBigUint64(4, true) !== token) return;
            clearTimeout(timer); channel.removeEventListener("message", listener); resolve(performance.now() - start);
          };
          const timer = setTimeout(() => { channel.removeEventListener("message", listener); reject(new Error(`${label} ACK timeout`)); }, 2000);
          channel.addEventListener("message", listener); channel.send(data);
        }));
      }
      samples.sort((a,b) => a-b);
      latency[label] = { samples: samples.length, medianMs: samples[20], p95Ms: samples[38] };
    }
    return { video: await route(window.__dual.video), input: await route(window.__dual.input), latency };
  });
  assert.notEqual(result.video.remotePort, result.input.remotePort, "native Host must use independent UDP sockets");
  assert.notEqual(result.video.localPort, result.input.localPort, "browser must use independent UDP sockets");
  await page.waitForTimeout(32_000);
  assert.equal(await page.evaluate(() => window.__dual.socket.readyState), 1, "idle proxy closed the signaling socket");
  assert.equal(await page.evaluate(() => window.__dual.video.connectionState), "connected");
  await page.evaluate(() => window.__dual.input.close());
  await page.waitForFunction(() => document.querySelector("video")?.dataset.inputTransport === "websocket-control");
  assert.equal(await page.evaluate(() => window.__dual.video.connectionState), "connected");
  await page.evaluate(async () => {
    const data = new Uint8Array(16), view = new DataView(data.buffer);
    view.setUint8(0, 5); view.setUint8(1, 1); view.setUint16(2, 4, true);
    view.setBigUint64(4, BigInt(Math.round((performance.timeOrigin + performance.now()) * 1000)), true);
    await new Promise((resolve, reject) => {
      const listener = ({data: raw}) => {
        const message = JSON.parse(raw);
        if (message.type !== "input_ack" || message.session_id !== window.__dual.session) return;
        clearTimeout(timer); window.__dual.socket.removeEventListener("message", listener); resolve();
      };
      const timer = setTimeout(() => { window.__dual.socket.removeEventListener("message", listener); reject(new Error("fallback ACK timeout")); }, 2000);
      window.__dual.socket.addEventListener("message", listener);
      window.__dual.socket.send(JSON.stringify({ type: "input_packet", session_id: window.__dual.session, data: [...data] }));
    });
  });
  await page.locator("#back").click();
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ ...result, heartbeat: "survived 32s behind a 20s idle-timeout proxy", fallback: "websocket-control; video remained connected" }, null, 2));
} catch (error) {
  console.error(logs.join("").split("\n").filter(line => /error|warn|failed/i.test(line) && !/browser client report|ticket|token/i.test(line)).slice(-20).join("\n"));
  throw error;
} finally {
  await browser?.close();
  for (const socket of proxySockets) socket.destroy();
  if (proxy) await new Promise(resolve => proxy.close(resolve));
  for (const child of children) if (child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
  const resolved = path.resolve(temp), parent = path.resolve(os.tmpdir());
  assert.equal(path.dirname(resolved), parent);
  assert.ok(path.basename(resolved).startsWith("remote-dual-input-"));
  fs.rmSync(resolved, { recursive: true, force: true });
}
