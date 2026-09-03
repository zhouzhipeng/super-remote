import fs from "node:fs";
import { createRequire } from "node:module";

const packagePath = process.env.PLAYWRIGHT_PACKAGE;
const executablePath = process.env.CHROME_EXECUTABLE;
const statusPath = process.env.REMOTE_STATUS_PATH;
const screenshotPath = process.env.REMOTE_SCREENSHOT_PATH;
if (!packagePath || !executablePath || !statusPath) {
  throw new Error("PLAYWRIGHT_PACKAGE, CHROME_EXECUTABLE and REMOTE_STATUS_PATH are required");
}

const status = JSON.parse(fs.readFileSync(statusPath, "utf8"));
const require = createRequire(import.meta.url);
const { chromium } = require(packagePath);
const browser = await chromium.launch({
  executablePath,
  headless: true,
  args: ["--no-sandbox", "--disable-gpu", "--disable-background-networking"],
});

async function instrument(context) {
  await context.addInitScript(() => {
    const NativePeerConnection = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends NativePeerConnection {
      constructor(configuration) {
        super(configuration);
        window.__takeoverPeer = this;
        window.__takeoverConfiguration = configuration;
      }
    };
  });
  const page = await context.newPage();
  const directUrl = new URL(status.direct_url);
  directUrl.searchParams.set("takeover-test", `${Date.now()}-${Math.random()}`);
  await page.goto(directUrl.href, { waitUntil: "domcontentloaded" });
  return page;
}

async function waitForVideo(page) {
  await page.waitForFunction(() => {
    const video = document.querySelector("video");
    return video && video.readyState >= 2 && video.videoWidth > 0 && video.currentTime > 0;
  }, null, { timeout: 40_000 });
}

async function snapshot(page) {
  return page.evaluate(async () => {
    const peer = window.__takeoverPeer;
    const report = await peer.getStats();
    const values = [...report.values()];
    const inbound = values.find((item) => item.type === "inbound-rtp" && item.kind === "video");
    const transport = values.find((item) => item.type === "transport");
    const selectedPair = report.get(transport?.selectedCandidatePairId)
      ?? values.find((item) => item.type === "candidate-pair" && item.state === "succeeded" && item.nominated);
    const selectedLocal = report.get(selectedPair?.localCandidateId);
    const video = document.querySelector("video");
    return {
      connectionState: peer.connectionState,
      iceConnectionState: peer.iceConnectionState,
      policy: window.__takeoverConfiguration?.iceTransportPolicy,
      selectedLocal: selectedLocal ? {
        type: selectedLocal.candidateType,
        protocol: selectedLocal.protocol,
        relayProtocol: selectedLocal.relayProtocol,
        address: selectedLocal.address,
        port: selectedLocal.port,
      } : null,
      video: {
        width: video.videoWidth,
        height: video.videoHeight,
        currentTime: video.currentTime,
        framesDecoded: inbound?.framesDecoded ?? 0,
        bytesReceived: inbound?.bytesReceived ?? 0,
      },
    };
  });
}

try {
  // Chromium with a Safari UA exercises the ordinary direct path and stays
  // active while the second, real Chrome-UA client takes ownership.
  const firstContext = await browser.newContext({
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15",
  });
  const firstPage = await instrument(firstContext);
  await waitForVideo(firstPage);
  const firstBefore = await snapshot(firstPage);

  const secondContext = await browser.newContext();
  const secondPage = await instrument(secondContext);
  await waitForVideo(secondPage);
  await secondPage.waitForTimeout(2_000);
  const second = await snapshot(secondPage);
  await firstPage.waitForFunction(() => window.__takeoverPeer?.connectionState === "closed", null, { timeout: 5_000 });
  const firstAfter = await firstPage.evaluate(() => ({
    connectionState: window.__takeoverPeer?.connectionState,
    disconnectDialogOpen: document.querySelector(".disconnect-dialog")?.open ?? false,
    disconnectMessage: document.querySelector("#disconnect-message")?.textContent ?? "",
  }));
  const result = { firstBefore, firstAfter, second };
  console.log(JSON.stringify(result));
  if (screenshotPath) await secondPage.screenshot({ path: screenshotPath });
  if (
    firstBefore.connectionState !== "connected"
    || firstAfter.connectionState !== "closed"
    || !firstAfter.disconnectDialogOpen
    || second.connectionState !== "connected"
    || second.policy !== "all"
    || !["host", "srflx", "prflx", "relay"].includes(second.selectedLocal?.type)
    || !(second.video.framesDecoded > 0)
  ) {
    process.exitCode = 1;
  }
} finally {
  await browser.close();
}
