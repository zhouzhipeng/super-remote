// Live audio regression. Uses a generated tone only; never records system audio.
// Takes ownership of the installed remote session, leaving host output muted.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { createRequire } from "node:module";
assert.equal(process.env.REMOTE_LIVE_TEST, "1");
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright");
const status = JSON.parse(fs.readFileSync(process.env.REMOTE_STATUS_PATH, "utf8"));
const endpoint = () => {
  const result = spawnSync(process.env.AUDIO_ENDPOINT_TOOL, [], { encoding:"utf8", windowsHide:true });
  assert.equal(result.status, 0); return JSON.parse(result.stdout);
};
const temp = fs.mkdtempSync(path.join(os.tmpdir(), "remote-audio-check-"));
const wav = path.join(temp, "tone.wav");
const rate = 48000, count = rate * 8, data = Buffer.alloc(44 + count * 2);
data.write("RIFF"); data.writeUInt32LE(data.length - 8,4); data.write("WAVEfmt ",8);
data.writeUInt32LE(16,16); data.writeUInt16LE(1,20); data.writeUInt16LE(1,22);
data.writeUInt32LE(rate,24); data.writeUInt32LE(rate*2,28); data.writeUInt16LE(2,32);
data.writeUInt16LE(16,34); data.write("data",36); data.writeUInt32LE(count*2,40);
for (let i=0;i<count;i++) data.writeInt16LE(Math.round(Math.sin(i*2*Math.PI*440/rate)*5000),44+i*2);
fs.writeFileSync(wav,data);
let browser, player;
try {
  browser = await chromium.launch({ executablePath:process.env.CHROME_EXECUTABLE,headless:true });
  const page = await browser.newPage();
  await page.addInitScript(() => {
    const Native = RTCPeerConnection;
    window.RTCPeerConnection = class extends Native {
      addTransceiver(kind, options) { if (kind === "audio") window.__audioPeer = this; return super.addTransceiver(kind,options); }
    };
  });
  await page.goto(status.direct_url).catch(() => { throw new Error("Live endpoint unavailable; URL omitted"); });
  await page.waitForFunction(() => window.__audioPeer?.connectionState === "connected" && document.querySelector("video")?.srcObject?.getAudioTracks().length, null, {timeout:60000});
  // Do not feed received audio back into this same machine's loopback capture.
  await page.evaluate(() => { document.querySelector("video").volume = 0; });
  await page.locator("#sound").click();
  await page.waitForFunction(() => !document.querySelector("video").muted);
  await page.evaluate(async () => {
    const video = document.querySelector("video");
    const ctx = new AudioContext(); await ctx.resume();
    const source = ctx.createMediaStreamSource(new MediaStream(video.srcObject.getAudioTracks()));
    const analyser = ctx.createAnalyser(); analyser.fftSize=2048; source.connect(analyser);
    window.__audioProbe = {ctx,source,analyser};
  });
  for (let i=0;i<30 && !endpoint().muted;i++) await page.waitForTimeout(200);
  assert.equal(endpoint().muted,true,"host policy failed to mute CURRENT endpoint");
  player = spawn("powershell.exe",["-NoProfile","-Command",'$p = New-Object System.Media.SoundPlayer; $p.SoundLocation = $env:REMOTE_TEST_WAV; $p.PlaySync()'],
    { windowsHide:true, env:{...process.env,REMOTE_TEST_WAV:wav},stdio:"ignore" });
  const result = await page.evaluate(async () => {
    let peakRms=0;
    const samples=new Float32Array(2048);
    for(let i=0;i<60;i++) {
      window.__audioProbe.analyser.getFloatTimeDomainData(samples);
      peakRms=Math.max(peakRms,Math.sqrt(samples.reduce((sum,x)=>sum+x*x,0)/samples.length));
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    const stats=[...(await window.__audioPeer.getStats()).values()];
    const audio=stats.find(r=>r.type==="inbound-rtp" && r.kind==="audio");
    return {peakRms,bytes:audio?.bytesReceived,packets:audio?.packetsReceived,
      muted:document.querySelector("video").muted,paused:document.querySelector("video").paused};
  });
  assert.ok(result.peakRms>0.01,"browser decoded only silence while host was muted");
  assert.ok(result.packets>100); assert.equal(result.muted,false); assert.equal(result.paused,false);
  assert.equal(endpoint().muted,true);
  await page.locator("#sound").click();
  assert.equal(await page.evaluate(()=>document.querySelector("video").muted),true);
  await page.evaluate(()=>window.__audioProbe.ctx.close());
  await page.locator("#back").click();
  console.log(JSON.stringify({...result,hostMuted:true,toggle:"unmute and mute passed"},null,2));
} finally {
  if(player && player.exitCode===null) player.kill();
  await browser?.close();
  assert.equal(path.dirname(path.resolve(temp)),path.resolve(os.tmpdir()));
  assert.ok(path.basename(temp).startsWith("remote-audio-check-"));
  fs.rmSync(temp,{recursive:true,force:true});
}
