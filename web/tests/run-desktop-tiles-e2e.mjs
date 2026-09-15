// Isolated browser fixture: no installed Host, credentials or real input.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { createServer } from "vite";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const server = await createServer({ root: fileURLToPath(new URL("..", import.meta.url)), server: { host: "127.0.0.1", port: 0 } });
await server.listen();
const browser = await chromium.launch({ executablePath: process.env.CHROME_EXECUTABLE, headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
  await page.route("**/tile-fixture", route => route.fulfill({ contentType: "text/html", body:
    '<link rel="stylesheet" href="/src/style.css"><main class="remote"><video></video><div style="height:48px"></div></main>' }));
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/tile-fixture`);
  const result = await page.evaluate(async () => {
    const { DesktopTiles } = await import("/src/desktop-tiles.ts");
    class Channel extends EventTarget {
      sent = []; closed = false;
      send(data) { this.sent.push(data); }
      close() { this.closed = true; }
      receive(data) { this.dispatchEvent(new MessageEvent("message", { data })); }
    }
    const channel = new Channel(), video = document.querySelector("video");
    channel.readyState = "open";
    let commits = 0;
    const display = new DesktopTiles(video, channel, () => { commits++; });
    channel.dispatchEvent(new Event("open"));
    if (channel.sent.length !== 1) throw new Error("Refinement began before video had a frame");
    Object.defineProperty(video,"readyState",{configurable:true,value:2});
    Object.defineProperty(video,"videoWidth",{configurable:true,value:130});
    video.dispatchEvent(new Event("playing"));
    video.dispatchEvent(new Event("playing"));
    if (channel.sent.filter(v=>v === "video-ready").length !== 1) throw new Error("Video readiness handshake duplicated or missing");
    const wait = async predicate => {
      const start = performance.now();
      while (!predicate()) { if (performance.now() - start > 5000) throw new Error("commit timeout"); await new Promise(r => setTimeout(r, 5)); }
    };
    const png = async (w, h, color) => {
      const canvas = document.createElement("canvas"); canvas.width = w; canvas.height = h;
      const context = canvas.getContext("2d"); context.fillStyle = color; context.fillRect(0,0,w,h);
      return new Uint8Array(await (await new Promise(r => canvas.toBlob(r, "image/png"))).arrayBuffer());
    };
    const packet = async (x,y,w,h,color) => {
      const image = await png(w,h,color), data = new Uint8Array(12 + image.length), view = new DataView(data.buffer);
      [x,y,w,h].forEach((value,i) => view.setUint16(i * 2,value,true)); view.setUint32(8,image.length,true); data.set(image,12); return data;
    };
    const send = (id,w,h,parts,copies=[]) => {
      const data = new Uint8Array(parts.reduce((n,p) => n+p.length,0)); let offset = 0;
      for (const part of parts) { data.set(part,offset); offset += part.length; }
      channel.receive(JSON.stringify({ type:"begin",id,width:w,height:h,bytes:data.length,tiles:parts.length,copies }));
      // Deliberately split inside headers and PNG bytes.
      for (let offset=0; offset<data.length; offset+=17) channel.receive(data.slice(offset,offset+17).buffer);
      channel.receive(JSON.stringify({type:"end",id}));
    };
    send(1,130,2,[await packet(0,0,128,2,"rgb(17,34,51)"),await packet(128,0,2,2,"rgb(17,34,51)")]);
    await wait(() => commits === 1);
    send(2,130,2,[await packet(128,0,2,2,"rgb(1,2,255)")]);
    await wait(() => commits === 2);
    const canvas = document.querySelector(".desktop-tiles"), context = canvas.getContext("2d");
    if (!canvas.hidden) throw new Error("Unvalidated snapshot became visible");
    channel.receive(JSON.stringify({type:"show",id:2,input:"0"}));
    const unchanged = [...context.getImageData(127,0,1,1).data], changed = [...context.getImageData(128,0,1,1).data];
    await new Promise(r => requestAnimationFrame(r));
    const aligned = Math.abs(canvas.getBoundingClientRect().height - video.getBoundingClientRect().height) < 1;
    const mode = video.dataset.displayTransport;
    video.dataset.latestInput = "10";
    video.dispatchEvent(new Event("remote-input"));
    if (!canvas.hidden) throw new Error("Local input did not immediately hide refinement");
    // Hiding is synchronous, but the retired bitmap fades on its own layer
    // instead of cutting straight to a downscaled video frame.
    const fade = document.querySelector(".desktop-tiles-fade");
    if (!fade || fade.hidden || !fade.getAnimations().length)
      throw new Error("Retracted refinement cut straight to video");
    // Both layers are desktop pixels and must share one stacking plane below
    // every overlay, the toolbar and the corner hint. Raising the fade layer
    // above .desktop-tiles makes it cover UI for the length of every fade.
    if (getComputedStyle(fade).zIndex !== getComputedStyle(canvas).zIndex)
      throw new Error("Fade layer left the desktop-pixel plane and can occlude UI");
    channel.receive(JSON.stringify({type:"show",id:2,input:"0"}));
    if (!canvas.hidden) throw new Error("Old snapshot covered newer input");
    channel.receive(JSON.stringify({type:"show",id:2,input:"10"}));
    if (canvas.hidden) throw new Error("Validated snapshot stayed hidden");
    if (!fade.hidden || fade.getAnimations().length)
      throw new Error("Fade layer outlived the sharp layer returning");
    channel.receive(JSON.stringify({type:"invalidate"}));
    if (!canvas.hidden) throw new Error("Remote change did not invalidate refinement");
    send(3,2,1,[await packet(0,0,2,1,"rgb(44,55,66)")]);
    await wait(() => commits === 3);
    if (canvas.width !== 130 || canvas.height !== 2) throw new Error("Decode cleared the canvas before resize validation");
    channel.receive(JSON.stringify({type:"show",id:3,input:"10"}));
    if (!canvas.getAnimations().length) throw new Error("Quality recovery has no smooth transition");
    const resized = [canvas.width,canvas.height,...context.getImageData(0,0,1,1).data];
    send(4,2,256,[await packet(0,0,2,128,"rgb(200,0,0)"),await packet(0,128,2,128,"rgb(0,0,200)")]);
    const copies = [{x:0,y:0,width:2,height:128,source_y:128},{x:0,y:128,width:2,height:128,source_y:0}];
    await wait(() => commits === 4);
    channel.receive(JSON.stringify({type:"show",id:4,input:"10",hidden:[1]}));
    if (context.getImageData(0,128,1,1).data[3] !== 0) throw new Error("Changed tile did not expose live video");
    if (context.getImageData(0,0,1,1).data[0] !== 200) throw new Error("Stable tile lost sharp pixels");
    send(5,2,256,[],copies);
    send(6,2,256,[],copies);
    await wait(() => commits === 6);
    if (canvas.hidden) throw new Error("Background commits flashed low-resolution video");
    if (context.getImageData(0,0,1,1).data[0] !== 200) throw new Error("Unvalidated update changed displayed pixels");
    channel.receive(JSON.stringify({type:"show",id:6,input:"10"}));
    const scrollRestored=[...context.getImageData(0,0,1,1).data,...context.getImageData(0,128,1,1).data];
    const { InputController } = await import("/src/input.ts");
    const fast = new Channel(), reliable = new Channel();
    fast.readyState = reliable.readyState = "open";
    fast.bufferedAmount = reliable.bufferedAmount = 0;
    video.setPointerCapture = () => {};
    const input = new InputController(video, fast, reliable, () => {}, () => {}, () => {}, () => ({text:""}), () => {});
    const box = video.getBoundingClientRect();
    const point = {clientX:box.x+box.width/2,clientY:box.y+box.height/2,pointerId:1};
    video.dispatchEvent(new WheelEvent("wheel",{...point,deltaY:120}));
    if (reliable.sent.length !== 2 || reliable.sent[0][0] !== 1 || reliable.sent[1][0] !== 4)
      throw new Error("First wheel did not position the remote pointer before scrolling");
    const pos = new DataView(reliable.sent[0].buffer, reliable.sent[0].byteOffset);
    if (Math.abs(pos.getUint16(12,true)-32768)>1 || Math.abs(pos.getUint16(14,true)-32768)>1)
      throw new Error("First wheel coordinates are incorrect");
    fast.sent.length=0; reliable.sent.length=0;
    video.dataset.latestInput="10";
    for (const event of [new PointerEvent("pointermove",point), new PointerEvent("pointerdown",point),
      new PointerEvent("pointerup",point),new WheelEvent("wheel",{deltaY:120}),
      new KeyboardEvent("keydown",{code:"KeyA",bubbles:true}),new KeyboardEvent("keyup",{code:"KeyA",bubbles:true})]) {
      delete video.dataset.wheelActiveUntil;
      channel.receive(JSON.stringify({type:"show",id:6,input:video.dataset.latestInput || "10"}));
      if (canvas.hidden) throw new Error("Input fixture did not show refinement");
      video.dispatchEvent(event);
      if (event.type === "pointermove") {
        if (canvas.hidden || video.dataset.latestInput !== "10") throw new Error("Pointer motion reduced quality");
      } else if (event.type === "keydown" || event.type === "keyup") {
        if (canvas.hidden) throw new Error("Typing reduced quality");
      } else if (!canvas.hidden) throw new Error(`${event.type} did not switch immediately to video`);
    }
    if (fast.sent.length !== 1 || reliable.sent.length !== 5) throw new Error("An input transition was lost");
    video.dispatchEvent(new WheelEvent("wheel",{deltaY:120}));
    channel.receive(JSON.stringify({type:"show",id:6,input:video.dataset.latestInput}));
    if (!canvas.hidden) throw new Error("Sharp overlay interrupted the active scroll window");
    delete video.dataset.wheelActiveUntil;
    input.destroy();
    channel.receive(JSON.stringify({type:"begin",id:7,width:256,height:128,bytes:100,tiles:2}));
    channel.receive(JSON.stringify({type:"cancel",id:7}));
    if (channel.closed) throw new Error("Cancellation closed the channel");
    send(8,2,256,[],copies);
    await wait(() => commits === 7);
    channel.receive(JSON.stringify({type:"begin",id:9,width:256,height:128,bytes:100,tiles:2}));
    channel.receive(JSON.stringify({type:"end",id:9}));
    return { unchanged,changed,aligned,mode,resized,scrollRestored,commits,closed:channel.closed,
      cleaned:!document.querySelector(".desktop-tiles") && !document.querySelector(".desktop-tiles-fade")
        && !video.dataset.desktopWidth,
      sent:channel.sent };
  });
  assert.deepEqual(result.unchanged,[17,34,51,255]);
  assert.deepEqual(result.changed,[1,2,255,255]);
  assert.deepEqual(result.resized,[2,1,44,55,66,255]);
  assert.equal(result.mode,"lossless-tiles");
  assert.ok(result.aligned && result.closed && result.cleaned);
  assert.equal(result.commits,7);
  assert.deepEqual(result.scrollRestored,[200,0,0,255,0,0,200,255]);
  assert.deepEqual(result.sent,["start","video-ready",... [1,2,3,4,5,6,8].map(id=>JSON.stringify({type:"ack",id}))]);
  console.log("PASS: lossless pixels, unchanged regions, chunk boundaries, resize, layout, commit ACK and failure cleanup");
} finally { await browser.close(); await server.close(); }
