// Real embedded Web UI + Rust signaling + authenticated bundled TURN, behind a
// raw TCP forwarder with a different, dynamically assigned external port.
// Only the Host is synthetic (canvas H.264); no installed service, desktop
// capture, input injection, persistent credentials, or public FRP changes.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { createRequire } from "node:module";
import net from "node:net";
import dgram from "node:dgram";
import { networkInterfaces } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const root = fileURLToPath(new URL("../..", import.meta.url));
const signalingExe = process.env.SIGNALING_EXECUTABLE || path.join(root, "target/debug/remote-signaling.exe");
const turnExe = process.env.TURN_EXECUTABLE;
assert.ok(turnExe, "TURN_EXECUTABLE must point to the bundled remote-turn.exe");
const lanIp = process.env.TEST_RELAY_IP;
assert.ok(lanIp, "TEST_RELAY_IP must be a local IPv4 address reachable by the synthetic Host");
assert.ok(Object.values(networkInterfaces()).flat().some((address) => address?.family === "IPv4" && address.address === lanIp),
  "TEST_RELAY_IP must belong to this test machine");
const children = [];
const sockets = new Set();
const log = [];
const signalTrace = [];
let browser;
let hostPage;
let clientPage;
let proxy;
let turnBytes = 0;
let turnConnections = 0;

async function listen(server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return server.address().port;
}

async function freePort() {
  const server = net.createServer();
  const port = await listen(server);
  await new Promise((resolve) => server.close(resolve));
  return port;
}

function start(exe, args, env) {
  const child = spawn(exe, args, { cwd: root, windowsHide: true, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  children.push(child);
  child.on("error", (error) => log.push(String(error)));
  for (const pipe of [child.stdout, child.stderr]) pipe.on("data", (chunk) => log.push(chunk.toString()));
  return child;
}

async function waitReady(url) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (children.some((child) => child.exitCode !== null)) throw new Error("isolated service exited");
    try { if ((await fetch(url, { signal: AbortSignal.timeout(500) })).ok) return; } catch { /* starting */ }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("isolated service did not start");
}

async function checkUnauthenticatedTurn(port) {
  const socket = net.connect(port, "127.0.0.1");
  sockets.add(socket);
  try {
    await once(socket, "connect");
    const request = Buffer.alloc(28);
    request.writeUInt16BE(3, 0); // Allocate
    request.writeUInt16BE(8, 2);
    request.writeUInt32BE(0x2112a442, 4);
    randomBytes(12).copy(request, 8);
    request.writeUInt16BE(0x0019, 20); // REQUESTED-TRANSPORT = UDP
    request.writeUInt16BE(4, 22);
    request[24] = 17;
    const response = new Promise((resolve, reject) => {
      let buffer = Buffer.alloc(0);
      socket.setTimeout(3000, () => reject(new Error("TURN authentication probe timed out")));
      socket.on("error", reject);
      socket.on("data", (chunk) => {
        buffer = Buffer.concat([buffer, chunk]);
        if (buffer.length >= 20 && buffer.length >= 20 + buffer.readUInt16BE(2)) resolve(buffer);
      });
    });
    socket.write(request);
    const packet = await response;
    assert.equal(packet.readUInt16BE(0), 0x0113, "unauthenticated TURN allocated an open relay");
    assert.ok(packet.subarray(8, 20).equals(request.subarray(8, 20)), "TCP mux corrupted STUN transaction");
    let errorCode;
    for (let offset = 20; offset + 4 <= packet.length;) {
      const type = packet.readUInt16BE(offset);
      const length = packet.readUInt16BE(offset + 2);
      if (type === 9) errorCode = packet[offset + 6] * 100 + packet[offset + 7];
      offset += 4 + Math.ceil(length / 4) * 4;
    }
    assert.equal(errorCode, 401, "TURN must require short-lived credentials");
  } finally { socket.destroy(); sockets.delete(socket); }
}

try {
  const webPort = await freePort();
  // Windows reserves different dynamic ranges for TCP and UDP (Hyper-V/VPN).
  // Ask UDP for a usable TURN port instead of recycling a TCP-only probe.
  const udpProbe = dgram.createSocket("udp4");
  udpProbe.bind(0, "0.0.0.0");
  await once(udpProbe, "listening");
  const turnUdpPort = udpProbe.address().port;
  udpProbe.close();
  const turnPort = await freePort();
  const deviceToken = randomBytes(24).toString("hex");
  const password = randomBytes(24).toString("hex");
  const secret = randomBytes(32).toString("hex");
  start(turnExe, ["--public-ip", lanIp, "--tcp-port", String(turnPort), "--udp-port", String(turnUdpPort),
    "--min-port", "53000", "--max-port", "53100"], { REMOTE_TURN_SECRET: secret });
  start(signalingExe, [], {
    REMOTE_BIND: `127.0.0.1:${webPort}`, REMOTE_ADMIN_USER: "frp-test", REMOTE_ADMIN_PASSWORD: password,
    REMOTE_JWT_SECRET: randomBytes(32).toString("hex"), REMOTE_DEVICE_TOKEN: deviceToken,
    REMOTE_TURN_SECRET: secret, REMOTE_TURN_URLS: `turn:${lanIp}:${turnPort}?transport=tcp`,
    REMOTE_TURN_TCP_BRIDGE: `127.0.0.1:${turnPort}`, RUST_LOG: "remote_signaling=info",
  });
  await waitReady(`http://127.0.0.1:${webPort}/api/healthz`);
  proxy = net.createServer((client) => {
    const upstream = net.connect(webPort, "127.0.0.1");
    let isTurn = false;
    client.once("data", (chunk) => { isTurn = chunk[0] === 0; if (isTurn) turnConnections++; });
    for (const socket of [client, upstream]) {
      sockets.add(socket);
      socket.setNoDelay(true);
      socket.on("data", (chunk) => { if (isTurn) turnBytes += chunk.length; });
      socket.on("close", () => { sockets.delete(socket); client.destroy(); upstream.destroy(); });
      socket.on("error", () => { client.destroy(); upstream.destroy(); });
    }
    client.pipe(upstream).pipe(client);
  });
  const publicPort = await listen(proxy);
  assert.notEqual(publicPort, webPort);
  const origin = `http://127.0.0.1:${publicPort}`;
  const muxUrl = `turn:127.0.0.1:${publicPort}?transport=tcp`;
  const unauthorized = await fetch(`${origin}/api/turn-credentials`);
  assert.equal(unauthorized.status, 401);
  await checkUnauthenticatedTurn(publicPort);

  browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 2 });
  const errors = [];
  const host = await context.newPage();
  hostPage = host;
  function traceSockets(page, label) {
    page.on("websocket", (socket) => {
      for (const direction of ["framesent", "framereceived"]) socket.on(direction, ({ payload }) => {
        try { signalTrace.push(`${label}:${direction}:${JSON.parse(String(payload)).type}`); } catch { /* non-signaling */ }
      });
    });
  }
  traceSockets(host, "host");
  host.on("pageerror", (error) => errors.push(`Host: ${error.message}`));
  await host.goto(`http://127.0.0.1:${webPort}/`);
  await host.evaluate(async ({ deviceToken, lanIp }) => {
    const deviceId = "synthetic-frp-host";
    window.__host = { channels: [], errors: [] };
    const response = await fetch("/api/device-ticket", {
      method: "POST", headers: { "x-device-token": deviceToken, "x-device-id": deviceId },
    });
    const { ticket } = await response.json();
    const socket = new WebSocket(`ws://${location.host}/api/ws?ticket=${ticket}`);
    const send = (message) => socket.send(JSON.stringify(message));
    let peer;
    let inputPeer;
    let stream;
    let drawTimer;
    let queue = Promise.resolve();
    await new Promise((resolve, reject) => {
      socket.onerror = () => reject(new Error("synthetic Host websocket failed"));
      socket.onopen = () => {
        send({ type: "device_register", device_id: deviceId, name: "Isolated FRP Host",
          capabilities: { width: 1280, height: 720, fps: 30, codecs: ["h264"], audio: false } });
        send({ type: "ping", nonce: 1 });
      };
      socket.onmessage = ({ data }) => {
        queue = queue.then(async () => {
          const signal = JSON.parse(data);
          if (signal.type === "pong") resolve();
          if (signal.type === "webrtc_offer") {
            peer = new RTCPeerConnection({ iceServers: [], bundlePolicy: "max-bundle" });
            window.__host.peer = peer;
            peer.onicecandidate = ({ candidate }) => {
              // The native Rust Host advertises literal interface IPs, not
              // Chromium mDNS names (relay-only browsers need not resolve mDNS).
              if (candidate) send({ type: "webrtc_ice", session_id: signal.session_id,
                candidate: candidate.candidate.replace(/\S+\.local\b/, lanIp),
                sdp_mid: null, sdp_mline_index: null, username_fragment: null });
            };
            peer.ondatachannel = ({ channel }) => {
              window.__host.channels.push(channel);
              if (channel.label === "frp-proof") channel.onmessage = ({ data }) => channel.send(data);
            };
            inputPeer = new RTCPeerConnection({ iceServers: [], bundlePolicy: "max-bundle" });
            inputPeer.onicecandidate = ({ candidate }) => {
              if (candidate) send({ type: "webrtc_ice", session_id: signal.session_id, input: true,
                candidate: candidate.candidate.replace(/\S+\.local\b/, lanIp),
                sdp_mid: null, sdp_mline_index: null, username_fragment: null });
            };
            inputPeer.ondatachannel = ({ channel }) => {
              window.__host.channels.push(channel);
              if (["frp-proof", "input-fast", "input-reliable"].includes(channel.label)) channel.onmessage = ({ data }) => channel.send(data);
              if (channel.label === "cursor") channel.onopen = () => channel.send(JSON.stringify({visible: true, shape: "text"}));
            };
            await inputPeer.setRemoteDescription({ type: "offer", sdp: signal.input_sdp });
            const inputAnswer = await inputPeer.createAnswer();
            await inputPeer.setLocalDescription(inputAnswer);
            const canvas = document.createElement("canvas");
            canvas.width = 1280; canvas.height = 720;
            const ctx = canvas.getContext("2d");
            let frame = 0;
            drawTimer = setInterval(() => {
              ctx.fillStyle = "#18232b"; ctx.fillRect(0, 0, 1280, 720);
              ctx.fillStyle = "#e9f2f7"; ctx.font = "32px monospace";
              ctx.fillText(`FRP TCP H.264 frame ${frame++}`, 40, 80);
            }, 1000 / 30);
            stream = canvas.captureStream(30);
            const sender = peer.addTrack(stream.getVideoTracks()[0], stream);
            const video = peer.getTransceivers().find((item) => item.receiver.track.kind === "video");
            video.setCodecPreferences(RTCRtpSender.getCapabilities("video").codecs.filter((codec) => codec.mimeType === "video/H264"));
            await peer.setRemoteDescription({ type: "offer", sdp: signal.sdp });
            const answer = await peer.createAnswer();
            window.__host.answerCodecs = answer.sdp.split("\r\n").filter((line) => /^m=|^a=rtpmap:/.test(line));
            window.__host.transceivers = peer.getTransceivers().map((item) => ({ mid: item.mid, sender: item.sender.track?.kind, receiver: item.receiver.track.kind }));
            await peer.setLocalDescription(answer);
            const parameters = sender.getParameters();
            parameters.degradationPreference = "maintain-resolution";
            for (const encoding of parameters.encodings) encoding.maxBitrate = 5_000_000;
            await sender.setParameters(parameters);
            send({ type: "webrtc_answer", session_id: signal.session_id, sdp: answer.sdp,
              input_sdp: inputAnswer.sdp, input_control: true, local_cursor: true });
          } else if (signal.type === "webrtc_ice") {
            await (signal.input ? inputPeer : peer).addIceCandidate({ candidate: signal.candidate, sdpMid: signal.sdp_mid ?? "0", sdpMLineIndex: signal.sdp_mline_index ?? 0 });
          } else if (signal.type === "input_packet") {
            send({ type: "input_ack", session_id: signal.session_id, data: signal.data });
          } else if (signal.type === "session_closed") {
            peer?.close(); inputPeer?.close(); stream?.getTracks().forEach((track) => track.stop()); clearInterval(drawTimer);
          } else if (signal.type === "error") throw new Error(signal.message);
        }).catch((error) => { window.__host.errors.push(error.message); reject(error); });
      };
    });
  }, { deviceToken, lanIp });

  const page = await context.newPage();
  clientPage = page;
  traceSockets(page, "client");
  page.on("pageerror", (error) => errors.push(`Client: ${error.message}`));
  await page.addInitScript(({ muxUrl }) => {
    const NativePeer = window.RTCPeerConnection;
    window.__frpTest = { errors: [], proof: "", peers: [], channels: {} };
    window.RTCPeerConnection = class extends NativePeer {
      addTransceiver(trackOrKind, init) {
        window.__frpTest.client = this;
        const transceiver = super.addTransceiver(trackOrKind, init);
        if (trackOrKind === "video") {
          // Restrict this synthetic call to the native Host's H.264 codec.
          // Chromium-to-Chromium can otherwise select VP8 for the canvas sender.
          transceiver.setCodecPreferences(RTCRtpReceiver.getCapabilities("video").codecs
            .filter((codec) => codec.mimeType === "video/H264"));
        }
        return transceiver;
      }
      createDataChannel(label, options) {
        const channel = super.createDataChannel(label, options);
        window.__frpTest.channels[label] = channel;
        if (label === "input-fast") window.__frpTest.input = this;
        return channel;
      }
      async setRemoteDescription(description) {
        try { return await super.setRemoteDescription(description); }
        catch (error) { window.__frpTest.errors.push(error.message); throw error; }
      }
      constructor(configuration) {
        // TEST ONLY: disable direct UDP, public STUN, and private TURN routes.
        // The URL/credentials must still come from the unmodified production API.
        const servers = configuration.iceServers.filter((server) => server.urls === muxUrl);
        if (servers.length !== 1) throw new Error("production Web client did not derive the FRP origin");
        super({ ...configuration, iceServers: servers, iceTransportPolicy: "relay" });
        window.__frpTest.peers.push(this);
        this.addEventListener("icecandidateerror", (event) => window.__frpTest.errors.push(`${event.errorCode}:${event.errorText}`));
        const proof = this.createDataChannel("frp-proof");
        proof.onopen = () => proof.send("authenticated TCP relay data channel");
        proof.onmessage = ({ data }) => { window.__frpTest.proof = data; };
      }
    };
  }, { muxUrl });
  await page.goto(origin);
  await page.locator('[name="username"]').fill("frp-test");
  await page.locator('[name="password"]').fill(password);
  await page.locator('button[type="submit"], form button').click();
  await page.locator('[data-device="synthetic-frp-host"]').click();
  await page.waitForFunction(() => {
    const video = document.querySelector("video");
    return video?.currentTime > 2 && window.__frpTest.proof && video.dataset.inputTransport === "webrtc-input"
      && video.style.cursor === "text" && document.querySelector(".connection-overlay").hidden;
  }, null, { timeout: 30_000 });
  const result = await page.evaluate(async () => {
    const peer = window.__frpTest.client;
    const stats = await peer.getStats();
    const reports = [...stats.values()];
    const transport = reports.find((item) => item.type === "transport");
    const pair = stats.get(transport.selectedCandidatePairId);
    const local = stats.get(pair.localCandidateId);
    const allocation = reports.find((item) => item.type === "local-candidate" && item.candidateType === "relay"
      && item.port === local.port && item.url === local.url && item.relayProtocol === "tcp");
    const inbound = reports.find((item) => item.type === "inbound-rtp" && item.kind === "video");
    const codec = stats.get(inbound.codecId);
    const inputStats = await window.__frpTest.input.getStats();
    const inputTransport = [...inputStats.values()].find(item => item.type === "transport" && item.selectedCandidatePairId);
    const inputPair = inputStats.get(inputTransport.selectedCandidatePairId);
    const inputLocal = inputStats.get(inputPair.localCandidateId);
    return { origin: location.origin, connection: peer.connectionState, ice: peer.iceConnectionState,
      candidateType: local.candidateType, relayProtocol: local.relayProtocol, turnUrl: local.url,
      usesTcpAllocation: Boolean(allocation), policy: peer.getConfiguration().iceTransportPolicy,
      codec: codec.mimeType, framesDecoded: inbound.framesDecoded, videoBytes: inbound.bytesReceived,
      inputPort: inputLocal.port, videoPort: local.port, inputRelayProtocol: inputLocal.relayProtocol,
      separatePeers: window.__frpTest.client !== window.__frpTest.input,
      inputTransport: document.querySelector("video").dataset.inputTransport,
      cursor: document.querySelector("video").style.cursor,
      dataChannel: window.__frpTest.proof, errors: window.__frpTest.errors };
  });
  assert.equal(result.connection, "connected");
  // Chromium may label a selected pair prflx when checking a local relay. It
  // must still be the same authenticated TCP allocation, never a direct path.
  assert.ok(["relay", "prflx"].includes(result.candidateType));
  assert.equal(result.policy, "relay");
  assert.equal(result.usesTcpAllocation, true);
  assert.equal(result.relayProtocol, "tcp");
  assert.equal(result.turnUrl, muxUrl);
  assert.equal(result.codec, "video/H264");
  assert.equal(result.separatePeers, true);
  assert.notEqual(result.inputPort, result.videoPort);
  assert.equal(result.inputRelayProtocol, "tcp");
  assert.equal(result.inputTransport, "webrtc-input");
  assert.equal(result.cursor, "text");
  assert.ok(result.framesDecoded >= 30);
  assert.ok(result.videoBytes > 0 && turnBytes > result.videoBytes);
  assert.ok(turnConnections >= 2, "both authentication and video must traverse the TCP mapping");
  assert.deepEqual(errors, []);
  assert.deepEqual(await host.evaluate(() => window.__host.errors), []);
  await page.evaluate(() => window.__frpTest.input.close());
  await page.waitForFunction(() => document.querySelector("video")?.dataset.inputTransport === "websocket-control");
  assert.equal(await page.evaluate(() => window.__frpTest.client.connectionState), "connected");
  console.log(JSON.stringify({ ...result, webPort, publicPort, turnConnections, turnBytes, unauthenticatedTurn: "401 rejected" }, null, 2));
} catch (error) {
  console.error("Signaling trace", signalTrace);
  if (clientPage) console.error("Client diagnostics", await clientPage.evaluate(async () => ({
    state: document.querySelector("#state")?.textContent,
    detail: document.querySelector("#connection-description")?.textContent,
    peer: window.__frpTest.client?.connectionState, ice: window.__frpTest.client?.iceConnectionState,
    signaling: window.__frpTest.client?.signalingState,
    errors: window.__frpTest.errors, proof: window.__frpTest.proof,
    stats: window.__frpTest.client ? [...(await window.__frpTest.client.getStats()).values()]
      .filter((item) => ["candidate-pair", "local-candidate", "remote-candidate", "inbound-rtp"].includes(item.type)) : [],
  })).catch(() => "page closed"));
  if (hostPage) console.error("Host diagnostics", await hostPage.evaluate(() => ({
    errors: window.__host.errors, peer: window.__host.peer?.connectionState,
    answerCodecs: window.__host.answerCodecs, transceivers: window.__host.transceivers,
    channels: window.__host.channels.map((channel) => ({ label: channel.label, state: channel.readyState })),
  })).catch(() => "page closed"));
  // Test secrets are freshly generated and kept out of diagnostics.
  console.error(log.join("").split("\n").filter((line) => !line.includes("browser client report"))
    .map((line) => line.replace(/(?:user|username)="[^"]*"/g, 'user="[test]"')).slice(-35).join("\n"));
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  if (proxy) await new Promise((resolve) => proxy.close(resolve));
  for (const child of children) {
    if (child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
  }
}
