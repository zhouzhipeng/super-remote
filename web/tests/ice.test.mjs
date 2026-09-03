import assert from "node:assert/strict";
import test from "node:test";

import {
  chromiumCompatibleIceServers,
  remoteIceCandidate,
  shouldUseChromiumLanCompatibility,
} from "../src/ice.ts";

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
