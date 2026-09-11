import assert from "node:assert/strict";
import test from "node:test";

import {
  browserIceServers,
  chromiumCompatibleIceServers,
  remoteIceCandidate,
  shouldUseChromiumLanCompatibility,
} from "../src/ice.ts";

const chromeAgent = "Mozilla/5.0 Chrome/153.0.0.0 Safari/537.36";
const safariAgent = "Mozilla/5.0 Version/18.5 Safari/605.1.15";
const credentials = {
  urls: ["turn:192.168.1.10:3478?transport=tcp"],
  username: "expiring-test-user", credential: "temporary-test-secret", tcp_mux: true,
};

test("Chrome derives its FRP TURN/TCP route from the current Web origin, not a fixed server", () => {
  for (const [page, expected] of [
    ["http://203.0.113.12:45678/remote#token=not-a-turn-password", "turn:203.0.113.12:45678?transport=tcp"],
    ["http://remote.example:32123/", "turn:remote.example:32123?transport=tcp"],
    ["http://remote.example/", "turn:remote.example:80?transport=tcp"],
    ["http://[2001:db8::1]:45678/", "turn:[2001:db8::1]:45678?transport=tcp"],
  ]) {
    const servers = browserIceServers(chromeAgent, page, credentials);
    assert.deepEqual(servers.at(-1), {
      urls: expected, username: credentials.username, credential: credentials.credential,
    });
    assert.ok(JSON.stringify(servers).includes("stun:stun.cloudflare.com:3478"));
    assert.ok(!JSON.stringify(servers).includes("not-a-turn-password"));
  }
});

test("does not advertise a TCP bridge absent on older/standalone servers", () => {
  for (const tcp_mux of [false, undefined]) {
    const servers = browserIceServers(chromeAgent, "http://remote.example:45678/", { ...credentials, tcp_mux });
    assert.equal(servers.length, 3);
    assert.ok(!JSON.stringify(servers).includes("remote.example"));
  }
});

test("does not guess raw TURN or TURN/TLS support from an HTTPS reverse proxy", () => {
  const servers = browserIceServers(chromeAgent, "https://remote.example/", credentials);
  assert.equal(servers.length, 3);
  assert.ok(!JSON.stringify(servers).includes("remote.example"));
});

test("Safari receives the same-origin FRP fallback alongside existing ICE routes", () => {
  assert.deepEqual(browserIceServers(safariAgent, "http://remote.example:45678/", credentials), [
    { urls: "stun:stun.l.google.com:19302" },
    { urls: credentials.urls, username: credentials.username, credential: credentials.credential },
    { urls: "turn:remote.example:45678?transport=tcp", username: credentials.username, credential: credentials.credential },
  ]);
});

test("Chrome retains two independent discovery servers when TURN is unavailable", () => {
  assert.deepEqual(browserIceServers(chromeAgent, "http://remote.example/"), [
    { urls: "stun:stun.l.google.com:19302" },
    { urls: "stun:stun.cloudflare.com:3478" },
  ]);
});

test("associates transport-level Host candidates with the bundled media section", () => {
  assert.deepEqual(remoteIceCandidate({
    type: "webrtc_ice",
    session_id: "session",
    candidate: "candidate:1 1 udp 1 192.0.2.10 50000 typ host",
    sdp_mid: null,
    sdp_mline_index: null,
    username_fragment: null,
  }), {
    candidate: "candidate:1 1 udp 1 192.0.2.10 50000 typ host",
    sdpMid: "0",
    sdpMLineIndex: 0,
    usernameFragment: undefined,
  });
});

test("preserves an explicit media association", () => {
  const candidate = remoteIceCandidate({
    type: "webrtc_ice",
    session_id: "session",
    candidate: "candidate:2 1 udp 1 198.51.100.10 50001 typ srflx",
    sdp_mid: "video",
    sdp_mline_index: 2,
    username_fragment: "ufrag",
  });
  assert.equal(candidate.sdpMid, "video");
  assert.equal(candidate.sdpMLineIndex, 2);
  assert.equal(candidate.usernameFragment, "ufrag");
});

test("recognizes desktop Chromium for the LAN compatibility path", () => {
  assert.equal(shouldUseChromiumLanCompatibility("Mozilla/5.0 Chrome/151.0.0.0 Safari/537.36"), true);
  assert.equal(shouldUseChromiumLanCompatibility("Mozilla/5.0 HeadlessChrome/151.0.0.0 Safari/537.36"), true);
  assert.equal(shouldUseChromiumLanCompatibility("Mozilla/5.0 Edg/151.0.0.0"), true);
  assert.equal(shouldUseChromiumLanCompatibility("Mozilla/5.0 Version/18.5 Safari/605.1.15"), false);
  assert.equal(shouldUseChromiumLanCompatibility("Mozilla/5.0 CriOS/151.0.0.0 Mobile/15E148 Safari/604.1"), false);
});

test("preserves Chromium STUN discovery while isolating and ordering TURN transports", () => {
  const servers = [
    { urls: "stun:stun.l.google.com:19302" },
    {
      urls: [
        "turn:192.168.0.115:3478?transport=tcp",
        "turn:192.168.0.115:3478?transport=udp",
      ],
      username: "user",
      credential: "secret",
    },
  ];
  assert.deepEqual(chromiumCompatibleIceServers(servers), [
    {
      urls: "stun:stun.l.google.com:19302",
    },
    {
      urls: "turn:192.168.0.115:3478?transport=udp",
      username: "user",
      credential: "secret",
    },
    {
      urls: "turn:192.168.0.115:3478?transport=tcp",
      username: "user",
      credential: "secret",
    },
  ]);
});

test("Chromium compatibility preserves STUN when TURN is unavailable", () => {
  assert.deepEqual(chromiumCompatibleIceServers([
    { urls: "stun:stun.l.google.com:19302" },
  ]), [
    { urls: "stun:stun.l.google.com:19302" },
  ]);
});
