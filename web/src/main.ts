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
  app.innerHTML = `<main class="remote"><video id="remote" autoplay muted playsinline tabindex="0"></video><div class="toolbar"><button id="back" class="secondary">断开</button><button id="sound">开启声音</button><button id="fullscreen" class="secondary" aria-pressed="false">全屏</button><button id="toolbar-pin" class="secondary" aria-pressed="true">取消固定</button><span id="state">正在连接</span><span id="stats"></span></div></main>`;
  const remote = app.querySelector<HTMLElement>(".remote")!;
  const video = app.querySelector<HTMLVideoElement>("#remote")!;
  const toolbar = app.querySelector<HTMLElement>(".toolbar")!;
  const pinButton = app.querySelector<HTMLButtonElement>("#toolbar-pin")!;
  const fullscreenButton = app.querySelector<HTMLButtonElement>("#fullscreen")!;
  const state = app.querySelector<HTMLSpanElement>("#state")!;
  const stats = app.querySelector<HTMLElement>("#stats")!;
  let disposed = false;
  const createRemoteSession = (): RemoteSession => {
    const next = new RemoteSession(video, stats);
    next.addEventListener("state", (event) => { state.textContent = stateLabel((event as CustomEvent<string>).detail); });
    next.addEventListener("error", (event) => { state.textContent = String((event as CustomEvent).detail); });
    return next;
  };
  let session = createRemoteSession();
  let connectedArea = physicalVideoArea(video);
  // Browser chrome, rotation and fullscreen can all change the number of physical
  // pixels available to the video. Debounce a reconnect so the Host restarts the
  // encoder at the new fitted resolution instead of stretching an old stream.
  let resizeTimer = 0;
  let forceResize = false;
  const scheduleResizeReconnect = (force = false): void => {
    forceResize ||= force;
    window.clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => {
      if (disposed || session.state !== "connected") return;
      const area = physicalVideoArea(video);
      const forced = forceResize;
      forceResize = false;
      if (!forced && Math.abs(area.width - connectedArea.width) < 64 && Math.abs(area.height - connectedArea.height) < 64) return;
      connectedArea = area;
      session.close();
      session = createRemoteSession();
      void session.connect(deviceId).catch((error) => { state.textContent = error instanceof Error ? error.message : String(error); });
    }, force ? 250 : 700);
  };
  const resizeObserver = new ResizeObserver(() => { scheduleResizeReconnect(); });
  resizeObserver.observe(video);

  let toolbarPinned = localStorage.getItem("remote-toolbar-pinned") !== "false";
  let toolbarHideTimer = 0;
  const setToolbarVisible = (visible: boolean, autoHide = false): void => {
    window.clearTimeout(toolbarHideTimer);
    if (toolbarPinned) return;
    remote.classList.toggle("toolbar-visible", visible);
    if (visible && autoHide) {
      toolbarHideTimer = window.setTimeout(() => {
        if (!toolbar.matches(":hover") && !toolbar.contains(document.activeElement)) {
          remote.classList.remove("toolbar-visible");
        }
      }, 2600);
    }
  };
  const applyToolbarMode = (pinned: boolean, reconnect: boolean): void => {
    toolbarPinned = pinned;
    localStorage.setItem("remote-toolbar-pinned", String(pinned));
    remote.classList.toggle("toolbar-unpinned", !pinned);
    remote.classList.remove("toolbar-visible");
    pinButton.textContent = pinned ? "取消固定" : "固定控制条";
    pinButton.title = pinned ? "取消固定，让画面占满整个窗口" : "将控制条固定在窗口底部";
    pinButton.setAttribute("aria-pressed", String(pinned));
    if (reconnect) {
      requestAnimationFrame(() => requestAnimationFrame(() => scheduleResizeReconnect(true)));
    }
  };
  applyToolbarMode(toolbarPinned, false);
  pinButton.addEventListener("click", () => { applyToolbarMode(!toolbarPinned, true); });
  remote.addEventListener("pointermove", (event) => {
    if (!toolbarPinned && event.clientX <= 64 && event.clientY <= 64) {
      setToolbarVisible(true, true);
    }
  });
  toolbar.addEventListener("pointerenter", () => { window.clearTimeout(toolbarHideTimer); });
  toolbar.addEventListener("pointerleave", () => { setToolbarVisible(true, true); });

  const updateFullscreenButton = (): void => {
    const fullscreen = document.fullscreenElement === remote;
    fullscreenButton.textContent = fullscreen ? "退出全屏" : "全屏";
    fullscreenButton.title = fullscreen ? "退出浏览器全屏" : "进入浏览器全屏";
    fullscreenButton.setAttribute("aria-pressed", String(fullscreen));
  };
  const onFullscreenChange = (): void => {
    updateFullscreenButton();
    requestAnimationFrame(() => scheduleResizeReconnect(true));
  };
  document.addEventListener("fullscreenchange", onFullscreenChange);
  fullscreenButton.addEventListener("click", () => {
    void (document.fullscreenElement ? document.exitFullscreen() : remote.requestFullscreen())
      .catch((error) => { state.textContent = `无法切换全屏：${error instanceof Error ? error.message : String(error)}`; });
  });
  updateFullscreenButton();

  app.querySelector("#back")!.addEventListener("click", () => {
    disposed = true;
    resizeObserver.disconnect();
    window.clearTimeout(resizeTimer);
    window.clearTimeout(toolbarHideTimer);
    document.removeEventListener("fullscreenchange", onFullscreenChange);
    if (document.fullscreenElement === remote) void document.exitFullscreen();
    session.close();
    void devicesView();
  });
  const soundButton = app.querySelector<HTMLButtonElement>("#sound")!;
  soundButton.addEventListener("click", () => {
    const muted = !video.muted;
    void session.setMuted(muted).then(() => {
      soundButton.textContent = muted ? "开启声音" : "静音";
    }).catch(() => { state.textContent = "浏览器阻止了声音播放，请再点一次"; });
  });
  try { await session.connect(deviceId); } catch (error) { state.textContent = error instanceof Error ? error.message : String(error); }
}

function physicalVideoArea(video: HTMLVideoElement): { width: number; height: number } {
  const rect = video.getBoundingClientRect();
  const pixelRatio = Math.max(1, window.devicePixelRatio || 1);
  return { width: Math.floor(rect.width * pixelRatio), height: Math.floor(rect.height * pixelRatio) };
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
