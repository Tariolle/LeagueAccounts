// The packaged app serves the UI under the CSP in tauri.conf.json, and Tauri
// gives every <style> element (and remote script) a nonce, which makes browsers
// ignore 'unsafe-inline' for that directive. Browser previews apply no CSP, so
// this serves the built `dist` the way Tauri does and fails on anything blocked.
import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const DIST = normalize(fileURLToPath(new URL("../dist/", import.meta.url)));
const config = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const TYPES = {
  ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".json": "application/json",
  ".png": "image/png", ".jpg": "image/jpeg", ".svg": "image/svg+xml", ".otf": "font/otf",
};

test("the packaged CSP allows OP.GG champion portraits", () => {
  const imgSources = config.app.security.csp.split(";")
    .map(part => part.trim().split(/\s+/))
    .find(([directive]) => directive === "img-src")
    ?.slice(1) ?? [];
  assert.ok(imgSources.includes("https://opgg-static.akamaized.net"));
});

/** Mirror tauri-utils `inject_nonce_token` and tauri's `replace_csp_nonce` for one page. */
function asTauriServesIt(html) {
  const nonces = { "style-src": [], "script-src": [] };
  const addNonce = (pattern, directive) => {
    html = html.replace(pattern, open => {
      const nonce = String(Math.floor(Math.random() * 1e15));
      nonces[directive].push(nonce);
      return `${open} nonce="${nonce}"`;
    });
  };
  addNonce(/<style(?![^>]*\bnonce=)/gi, "style-src");
  addNonce(/<script(?=[^>]*\bsrc=["']?http)(?![^>]*\bnonce=)/gi, "script-src");
  const directives = new Map(config.app.security.csp.split(";").map(part => part.trim()).filter(Boolean)
    .map(part => { const [name, ...sources] = part.split(/\s+/); return [name, sources]; }));
  for (const [directive, list] of Object.entries(nonces)) {
    if (!list.length) continue;
    const sources = directives.get(directive) ?? [];
    if (!sources.includes("'self'")) sources.push("'self'");
    sources.push(...list.map(nonce => `'nonce-${nonce}'`));
    directives.set(directive, sources);
  }
  return { html, csp: [...directives].map(([name, sources]) => [name, ...sources].join(" ")).join("; ") };
}

test("the packaged UI keeps its inline styles under Tauri's CSP", { timeout: 60000 }, async () => {
  await readFile(join(DIST, "index.html")).catch(() => assert.fail("Build the UI first (npm run build): this checks dist."));
  const server = createServer(async (request, response) => {
    const path = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    const file = normalize(join(DIST, path === "/" ? "index.html" : path));
    if (!file.startsWith(DIST)) return response.writeHead(403).end();
    try {
      let body = await readFile(file);
      const headers = { "Content-Type": TYPES[extname(file)] ?? "application/octet-stream" };
      if (extname(file) === ".html") {
        const page = asTauriServesIt(body.toString());
        body = page.html;
        headers["Content-Security-Policy"] = page.csp;
      }
      response.writeHead(200, headers).end(body);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const browser = await puppeteer.launch({
    executablePath: process.env.CHROME_PATH ?? "C:/Program Files/Google/Chrome/Application/chrome.exe",
  });
  try {
    const page = await browser.newPage();
    await page.evaluateOnNewDocument(() => {
      window.__violations = [];
      document.addEventListener("securitypolicyviolation", event => {
        window.__violations.push(`${event.violatedDirective}: ${event.sample || event.blockedURI}`);
      });
    });
    await page.goto(`http://127.0.0.1:${server.address().port}/`, { waitUntil: "networkidle0" });
    // Outside Tauri the app stops at its first backend call, after building the
    // static interface and its icons.
    await page.waitForSelector("#startup-error", { visible: true });
    assert.equal(await page.$eval("#startup-loader", el => getComputedStyle(el).backgroundColor), "rgb(6, 14, 23)");
    const icon = await page.$eval('.side-link[data-action="settings"] .native-icon', el => {
      const style = getComputedStyle(el);
      return { width: el.getBoundingClientRect().width, mask: style.maskImage || style.webkitMaskImage };
    });
    assert.ok(icon.width > 0);
    assert.match(icon.mask, /icon_settings\.png/);
    // Exercise the actual browser policy, rather than just searching its text.
    // A local fixture supplies the response so this stays deterministic/offline.
    const portraitUrl = "https://opgg-static.akamaized.net/meta/images/lol/16.20.1/champion/Zeri.png";
    const portrait = await readFile(join(DIST, "league/ranks/iron.png"));
    await page.setRequestInterception(true);
    page.on("request", request => {
      if (request.url() === portraitUrl) void request.respond({ status: 200, contentType: "image/png", body: portrait });
      else void request.continue();
    });
    assert.ok(await page.evaluate(async url => {
      const image = new Image();
      image.src = url;
      document.body.append(image);
      await image.decode();
      return image.naturalWidth;
    }, portraitUrl));
    assert.deepEqual(await page.evaluate(() => window.__violations), []);
  } finally {
    await browser.close();
    server.close();
  }
});
