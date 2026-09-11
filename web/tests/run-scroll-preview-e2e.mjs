// Synthetic canvas video only. Exercises actual browser frame callbacks,
// compositing, letterboxing and lifecycle without connecting to a desktop.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const root = fileURLToPath(new URL("../..", import.meta.url));
const server = spawn(process.execPath, ["web/node_modules/vite/bin/vite.js", "web", "--host", "127.0.0.1", "--port", "4187", "--strictPort"], { cwd: root, windowsHide: true, stdio: "ignore" });
let browser;
try {
  for (let i=0; i<100; i++) {
    try { if ((await fetch("http://127.0.0.1:4187/src/scroll-preview.ts")).ok) break; } catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
  const page = await browser.newPage({ viewport: { width: 900, height: 700 } });
  await page.route("http://127.0.0.1:4187/", route => route.fulfill({ contentType: "text/html", body: '<style>body{margin:0}.remote{position:relative;width:800px;height:600px}video{width:100%;height:100%;object-fit:contain}.scroll-preview{position:absolute;pointer-events:none;z-index:1}.scroll-preview[hidden]{display:none}</style><div class="remote"><video muted autoplay></video></div>' }));
  await page.goto("http://127.0.0.1:4187/");
  await page.evaluate(async () => {
    const { ScrollPreview } = await import("/src/scroll-preview.ts");
    const video = document.querySelector("video");
    const source = document.createElement("canvas"); source.width=2560; source.height=1536;
    const ctx = source.getContext("2d");
    ctx.scale(4,4);
    let shift=0;
    const draw = () => {
      ctx.fillStyle="#202020"; ctx.fillRect(0,0,640,384);
      for(let y=12;y<84;y++) for(let x=20;x<140;x++) {
        const gray=(x*731+(y-shift)*173+x*(y-shift)*31)&255;
        ctx.fillStyle=`rgb(${gray},${gray},${gray})`;ctx.fillRect(x*4,y*4,4,4);
      }
    };
    draw(); video.srcObject=source.captureStream(30); await video.play();
    const preview = new ScrollPreview(video);
    let ticks=0;
    const timer=setInterval(() => {
      video.dispatchEvent(new WheelEvent("wheel", {deltaY:30,clientX:400,clientY:300}));
      shift-=3; draw(); ticks++;
    }, 40);
    window.fixture={ preview, source, video, timer, get ticks(){return ticks;} };
  });
  await page.waitForFunction(() => document.querySelector("video").dataset.scrollPreview === "predicting"
    && !document.querySelector(".scroll-preview").hidden, null, {timeout:10_000});
  const layout=await page.evaluate(() => {
    const overlay=document.querySelector(".scroll-preview");
    return { left:overlay.style.left, top:overlay.style.top, width:overlay.style.width, height:overlay.style.height,
      pixelWidth:overlay.width, pixelHeight:overlay.height,
      pointerEvents:getComputedStyle(overlay).pointerEvents, ticks:window.fixture.ticks };
  });
  assert.equal(layout.top,"60px"); assert.equal(layout.width,"800px"); assert.equal(layout.height,"480px");
  assert.equal(layout.pointerEvents,"none");
  assert.equal(layout.pixelWidth,2560); assert.equal(layout.pixelHeight,1536);
  await page.evaluate(() => clearInterval(window.fixture.timer));
  await page.waitForTimeout(180);
  assert.equal(await page.locator(".scroll-preview").evaluate(el=>el.hidden),true,"stale predicted pixels must expire");
  await page.evaluate(() => { window.fixture.preview.enabled=false; window.fixture.preview.destroy(); window.fixture.video.srcObject.getTracks().forEach(t=>t.stop()); });
  assert.equal(await page.locator(".scroll-preview").count(),0);
  console.log(JSON.stringify({ prediction:"active", layout, staleFrameExpiry:"passed", cleanup:"passed" }));
} finally {
  await browser?.close();
  if (server.exitCode===null) {const exited=once(server,"exit");server.kill();await exited;}
}
