import type { ServerSignal } from "./types.ts";

type ServerIceSignal = Extract<ServerSignal, { type: "webrtc_ice" }>;

export type BrowserTurnCredentials = {
  urls: string[];
  username: string;
  credential: string;
  tcp_mux?: boolean;
};

export function browserIceServers(userAgent: string, pageUrl: string, turn?: BrowserTurnCredentials): RTCIceServer[] {
  const chromium = shouldUseChromiumLanCompatibility(userAgent);
  const servers: RTCIceServer[] = [{ urls: "stun:stun.l.google.com:19302" }];
  // Independent discovery fallback when the Google STUN hostname is blocked.
  if (chromium) servers.push({ urls: "stun:stun.cloudflare.com:3478" });
  if (!turn) return servers;
  servers.push({ urls: turn.urls, username: turn.username, credential: turn.credential });
  if (chromium && turn.tcp_mux === true) {
    const page = new URL(pageUrl);
    // A raw FRP TCP mapping carries TURN/TCP on the very same Web port. Derive
    // it from the address the browser actually opened, never a configured LAN
    // IP, public server IP, or fixed port. HTTP(S) reverse proxies cannot carry
    // raw TURN; do not assume an HTTPS origin also provides a TLS TURN service.
    if (page.protocol === "http:") {
      servers.push({
        urls: `turn:${page.hostname}:${page.port || "80"}?transport=tcp`,
        username: turn.username,
        credential: turn.credential,
      });
    }
  }
  return servers;
}

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
      // transport cannot suppress the others. Private TURN addresses are useful
      // on a LAN, but are not a reliable fallback for an Internet/FRP client.
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
