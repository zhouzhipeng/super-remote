import type { DeviceSummary } from "./types.ts";

const TOKEN_KEY = "remote.accessToken";

export function accessToken(): string | null {
  return sessionStorage.getItem(TOKEN_KEY);
}

export function setAccessToken(token: string): void {
  sessionStorage.setItem(TOKEN_KEY, token);
}

export function logout(): void {
  sessionStorage.removeItem(TOKEN_KEY);
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("content-type", "application/json");
  const token = accessToken();
  if (token) headers.set("authorization", `Bearer ${token}`);
  const response = await fetch(`/api${path}`, { ...init, headers });
  if (!response.ok) {
    const error = await response.json().catch(() => ({ message: response.statusText })) as { message?: string };
    throw new Error(error.message ?? `HTTP ${response.status}`);
  }
  return response.json() as Promise<T>;
}

export async function login(username: string, password: string): Promise<void> {
  const result = await request<{ access_token: string }>("/login", {
    method: "POST",
    body: JSON.stringify({ username, password }),
  });
  sessionStorage.setItem(TOKEN_KEY, result.access_token);
}

export function listDevices(): Promise<DeviceSummary[]> {
  return request("/devices");
}

export function createSession(deviceId: string): Promise<{ session_id: string; session_token: string }> {
  return request("/sessions", { method: "POST", body: JSON.stringify({ device_id: deviceId }) });
}

export async function websocketTicket(): Promise<string> {
  return (await request<{ ticket: string }>("/ws-ticket", { method: "POST", body: "{}" })).ticket;
}

export async function iceServers(): Promise<RTCIceServer[]> {
  const servers: RTCIceServer[] = [{ urls: "stun:stun.l.google.com:19302" }];
  try {
    const turn = await request<{ urls: string[]; username: string; credential: string }>("/turn-credentials");
    servers.push({ urls: turn.urls, username: turn.username, credential: turn.credential });
  } catch {
    // TURN is optional for local/LAN development. The server returns 404 when not configured.
  }
  return servers;
}

export async function reportClient(report: Record<string, unknown>): Promise<void> {
  await request<never>("/client-report", { method: "POST", body: JSON.stringify(report) });
}

/**
 * Read while the Cmd+C/Control+C key event is still on the JavaScript stack.
 * Async clipboard writes lose their trusted user gesture on plain HTTP LAN
 * origins, so this intentionally uses the browser's synchronous XHR primitive.
 */
export function localClipboardTextDuringGesture(): string {
  const token = accessToken();
  if (!token) throw new Error("登录已失效");
  const request = new XMLHttpRequest();
  request.open("GET", "/api/local-clipboard", false);
  request.setRequestHeader("authorization", `Bearer ${token}`);
  request.send();
  if (request.status < 200 || request.status >= 300) {
    throw new Error(`读取主机剪贴板失败（HTTP ${request.status}）`);
  }
  const response = JSON.parse(request.responseText) as { text?: unknown };
  if (typeof response.text !== "string") throw new Error("主机剪贴板响应无效");
  return response.text;
}
