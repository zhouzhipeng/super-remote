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
const directUrl = new URL(status.direct_url);
directUrl.searchParams.set("relay-test", String(Date.now()));
const require = createRequire(import.meta.url);
const { chromium } = require(packagePath);
const browser = await chromium.launch({
  executablePath,
  headless: true,
  args: ["--no-sandbox", "--disable-gpu", "--disable-background-networking"],
});
try {
  const context = await browser.newContext();
  await context.addInitScript(() => {
    const NativePeerConnection = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends NativePeerConnection {
      constructor(configuration) {
        super(configuration);
        window.__relayTestPeer = this;
        window.__relayTestConfiguration = configuration;
      }
    };
  });
  const page = await context.newPage();
  await page.goto(directUrl.href, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(() => {
    const video = document.querySelector("video");
    return video && video.readyState >= 2 && video.videoWidth > 0 && video.currentTime > 0;
  }, null, { timeout: 35_000 });
  await page.waitForTimeout(2_000);
  const result = await page.evaluate(async () => {
    const peer = window.__relayTestPeer;
    const report = await peer.getStats();
    const values = [...report.values()];
    const transport = values.find((item) => item.type === "transport");
    const selectedPair = report.get(transport?.selectedCandidatePairId)
      ?? values.find((item) => item.type === "candidate-pair" && item.state === "succeeded" && item.nominated);
    const local = report.get(selectedPair?.localCandidateId);
    const remote = report.get(selectedPair?.remoteCandidateId);
    const inbound = values.find((item) => item.type === "inbound-rtp" && item.kind === "video");
    const video = document.querySelector("video");
    return {
      state: peer.connectionState,
      iceState: peer.iceConnectionState,
      policy: window.__relayTestConfiguration?.iceTransportPolicy,
      selectedPair: selectedPair ? {
        state: selectedPair.state,
        nominated: selectedPair.nominated,
        requestsSent: selectedPair.requestsSent,
        responsesReceived: selectedPair.responsesReceived,
      } : null,
      localCandidate: local ? {
        type: local.candidateType,
        protocol: local.protocol,
        relayProtocol: local.relayProtocol,
        address: local.address,
        port: local.port,
      } : null,
      remoteCandidate: remote ? {
        type: remote.candidateType,
        protocol: remote.protocol,
        address: remote.address,
        port: remote.port,
      } : null,
      video: {
        width: video.videoWidth,
        height: video.videoHeight,
        currentTime: video.currentTime,
        readyState: video.readyState,
        framesDecoded: inbound?.framesDecoded,
        bytesReceived: inbound?.bytesReceived,
      },
    };
  });
  console.log(JSON.stringify(result));
  if (screenshotPath) await page.screenshot({ path: screenshotPath });
  if (
    result.state !== "connected"
    || result.policy !== "all"
    || result.selectedPair?.state !== "succeeded"
    || !["host", "srflx", "prflx", "relay"].includes(result.localCandidate?.type)
    || !(result.video.framesDecoded > 0)
  ) {
    process.exitCode = 1;
  }
} finally {
  await browser.close();
}
