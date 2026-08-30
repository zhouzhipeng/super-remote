import "./style.css";
import { accessToken, listDevices, login, logout, setAccessToken } from "./api.ts";
import { RemoteSession } from "./rtc.ts";

const app = document.querySelector<HTMLDivElement>("#app")!;

function loginView(message = ""): void {
  app.innerHTML = `<main class="center"><form class="card login"><div class="brand">Remote Desktop</div><p>登录后连接你的 Windows 电脑</p><label>用户名<input name="username" autocomplete="username" required /></label><label>密码<input name="password" type="password" autocomplete="current-password" required /></label><button>登录</button><output>${escapeHtml(message)}</output></form></main>`;
  app.querySelector("form")!.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget as HTMLFormElement);
    try {
      await login(String(form.get("username")), String(form.get("password")));
      await devicesView();
    } catch (error) { loginView(error instanceof Error ? error.message : String(error)); }
  });
}

async function devicesView(): Promise<void> {
  try {
    const devices = await listDevices();
    app.innerHTML = `<main class="devices"><header><div class="brand">Remote Desktop</div><button class="secondary" id="logout">退出</button></header><section><h1>设备</h1><div class="device-grid">${devices.map((device) => `<article class="card"><div class="status ${device.online ? "online" : ""}"></div><h2>${escapeHtml(device.name)}</h2><p>${device.capabilities.width}×${device.capabilities.height} · ${device.capabilities.fps} FPS · ${device.capabilities.codecs.join(", ").toUpperCase()}</p><button data-device="${escapeHtml(device.id)}" ${device.online ? "" : "disabled"}>连接</button></article>`).join("") || `<p>还没有已注册设备。请先启动 Windows Host。</p>`}</div></section></main>`;
    app.querySelector("#logout")!.addEventListener("click", () => { logout(); loginView(); });
    app.querySelectorAll<HTMLButtonElement>("[data-device]").forEach((button) => button.addEventListener("click", () => sessionView(button.dataset.device!)));
  } catch { logout(); loginView("登录已失效，请重新登录"); }
}

async function sessionView(deviceId: string): Promise<void> {
  app.innerHTML = `<main class="remote"><video id="remote" autoplay muted playsinline tabindex="0"></video><div class="toolbar"><button id="back" class="secondary">断开</button><button id="sound">开启声音</button><button id="fullscreen" class="secondary">全屏</button><span id="state">正在连接</span><span id="stats"></span></div></main>`;
  const video = app.querySelector<HTMLVideoElement>("#remote")!;
  const state = app.querySelector<HTMLSpanElement>("#state")!;
  const session = new RemoteSession(video, app.querySelector("#stats")!);
  session.addEventListener("state", (event) => { state.textContent = stateLabel((event as CustomEvent<string>).detail); });
  session.addEventListener("error", (event) => { state.textContent = String((event as CustomEvent).detail); });
  app.querySelector("#back")!.addEventListener("click", () => { session.close(); void devicesView(); });
  const soundButton = app.querySelector<HTMLButtonElement>("#sound")!;
  soundButton.addEventListener("click", () => {
    const muted = !video.muted;
    void session.setMuted(muted).then(() => {
      soundButton.textContent = muted ? "开启声音" : "静音";
    }).catch(() => { state.textContent = "浏览器阻止了声音播放，请再点一次"; });
  });
  app.querySelector("#fullscreen")!.addEventListener("click", () => app.querySelector(".remote")?.requestFullscreen());
  try { await session.connect(deviceId); } catch (error) { state.textContent = error instanceof Error ? error.message : String(error); }
}

function stateLabel(state: string): string {
  return ({ creating_session: "创建会话", negotiating: "协商连接", connected: "已连接", reconnecting: "正在重连", closed: "已断开", idle: "空闲" } as Record<string, string>)[state] ?? state;
}

function escapeHtml(value: string): string {
  const div = document.createElement("div"); div.textContent = value; return div.innerHTML;
}

const quickLink = new URLSearchParams(location.hash.slice(1));
const quickToken = quickLink.get("token");
const quickDevice = quickLink.get("device");
if (quickToken) {
  setAccessToken(quickToken);
  history.replaceState(null, "", `${location.pathname}${location.search}`);
}

if (accessToken()) {
  if (quickDevice) void sessionView(quickDevice);
  else void devicesView();
} else loginView();
