#!/usr/bin/env node
// Export the slide deck to a 16:9 PDF with landscape pages and backgrounds.
//
// Chrome's --print-to-pdf uses the default paper width for layout, which trips
// the responsive breakpoint and clips the slides. This script drives Chrome over
// the DevTools Protocol instead, fixes the layout viewport at 1280x720 and prints
// with the deck's @page size (13.333in x 7.5in).
//
// Usage: node demos/slides/print_pdf.mjs [--chrome PATH] [--out FILE]

import { spawn } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");

const args = process.argv.slice(2);
const value = (flag) => {
  const index = args.indexOf(flag);
  return index >= 0 ? args[index + 1] : undefined;
};
const chrome = value("--chrome") || process.env.CHROME_BIN || "google-chrome";
const out = resolve(value("--out") || resolve(here, "av2codec-multiview-slides.pdf"));
const page = resolve(here, "index.html");
const profile = resolve(repo, "target/slides-pdf-profile");
const port = 9421;

rmSync(profile, { recursive: true, force: true });
mkdirSync(profile, { recursive: true });

const browser = spawn(
  chrome,
  [
    "--headless=new",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    `--user-data-dir=${profile}`,
    `--remote-debugging-port=${port}`,
    "about:blank",
  ],
  { stdio: "ignore" },
);

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function endpoint() {
  for (let attempt = 0; attempt < 60; attempt++) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`);
      const targets = await response.json();
      const target = targets.find((item) => item.type === "page");
      if (target) return target.webSocketDebuggerUrl;
    } catch {}
    await sleep(200);
  }
  throw new Error("Chrome did not expose a page target");
}

function client(url) {
  const socket = new WebSocket(url);
  const pending = new Map();
  let id = 0;
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    const callback = pending.get(message.id);
    if (!callback) return;
    pending.delete(message.id);
    if (message.error) callback.reject(new Error(message.error.message));
    else callback.resolve(message.result);
  };
  const ready = new Promise((resolveOpen, reject) => {
    socket.onopen = resolveOpen;
    socket.onerror = () => reject(new Error("CDP socket error"));
  });
  return {
    ready,
    send(method, params = {}, timeout = 30000) {
      return new Promise((resolveCall, reject) => {
        const messageId = ++id;
        const timer = setTimeout(() => {
          pending.delete(messageId);
          reject(new Error(`timeout: ${method}`));
        }, timeout);
        pending.set(messageId, {
          resolve: (result) => {
            clearTimeout(timer);
            resolveCall(result);
          },
          reject: (error) => {
            clearTimeout(timer);
            reject(error);
          },
        });
        socket.send(JSON.stringify({ id: messageId, method, params }));
      });
    },
    close() {
      socket.close();
    },
  };
}

try {
  const cdp = client(await endpoint());
  await cdp.ready;
  await cdp.send("Page.enable");
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: 1280,
    height: 720,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await cdp.send("Page.navigate", { url: `file://${page}` });
  await sleep(1200);
  const { data } = await cdp.send(
    "Page.printToPDF",
    {
      printBackground: true,
      preferCSSPageSize: true,
      paperWidth: 13.333,
      paperHeight: 7.5,
      marginTop: 0,
      marginBottom: 0,
      marginLeft: 0,
      marginRight: 0,
    },
    60000,
  );
  writeFileSync(out, Buffer.from(data, "base64"));
  cdp.close();
  console.log(`wrote ${out}`);
} finally {
  browser.kill("SIGTERM");
}
