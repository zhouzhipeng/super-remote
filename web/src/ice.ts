import type { ServerSignal } from "./types.ts";

type ServerIceSignal = Extract<ServerSignal, { type: "webrtc_ice" }>;

export function remoteIceCandidate(signal: ServerIceSignal): RTCIceCandidateInit {
  return {
    candidate: signal.candidate,
    // The Host's ICE library emits transport-level candidates without a media
    // association. Safari accepts that extension, while Chromium requires at
    // least one of sdpMid/sdpMLineIndex. All tracks use max-bundle, so m=0 is
    // the one shared ICE transport and is the correct standards-level target.
    sdpMid: signal.sdp_mid ?? "0",
    sdpMLineIndex: signal.sdp_mline_index ?? 0,
    usernameFragment: signal.username_fragment ?? undefined,
  };
}

export function shouldUseChromiumLanCompatibility(userAgent: string): boolean {
  const isDesktopChromium = /(?:\bChrome|HeadlessChrome|\bChromium|\bEdg|\bOPR)\//.test(userAgent)
    && !/\b(?:CriOS|EdgiOS|OPiOS)\//.test(userAgent);
  return isDesktopChromium;
}

export function chromiumCompatibleIceServers(servers: RTCIceServer[]): RTCIceServer[] {
  const isolatedServers = servers.flatMap((server) => {
    const urls = typeof server.urls === "string" ? [server.urls] : server.urls;
    return urls
      // Keep every discovery mechanism, but isolate each URL so a failing
      // transport cannot suppress the others. Real macOS Chrome 151 succeeds
      // on this network only after the public STUN server produces an srflx
      // candidate; removing STUN left one unusable mDNS host candidate because
      // that browser also declined to allocate either advertised TURN route.
      .map((url) => ({ ...server, urls: url }));
  });
  return isolatedServers.sort((left, right) => iceServerRank(String(left.urls)) - iceServerRank(String(right.urls)));
}

function iceServerRank(url: string): number {
  if (/^stuns?:/i.test(url)) return 0;
  if (/[?&]transport=udp(?:&|$)/i.test(url)) return 1;
  if (/[?&]transport=tcp(?:&|$)/i.test(url)) return 2;
  return 3;
}
