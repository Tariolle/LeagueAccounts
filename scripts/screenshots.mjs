// Capture README screenshots and GIFs from the real UI with synthetic data.
//
// Usage: `npm run screenshots`, or `npm run screenshots -- demo` for one scene.
// Starts an isolated dev server automatically.
// Requires Chrome and ffmpeg on PATH (GIFs are converted from screencasts).
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, renameSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { demoBrowser, demoPage } from "./demo-browser.mjs";

const OUT = resolve("docs/screenshots");
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const { version } = JSON.parse(readFileSync("package.json", "utf8"));

// The README shows the app, not the preview: hide the DEV DATA badge and Reset demo,
// and show the release version instead of the mock's.
async function capturePage(browser, base, scene) {
  const page = await demoPage(browser, base, scene);
  await page.evaluate((version) => {
    document.getElementById("dev-tools").hidden = true;
    document.getElementById("version").textContent = `v${version}`;
  }, version);
  return page;
}

// Static screens: scene -> wait for animations to settle.
const STILLS = { tft: 1000, list: 1000, settings: 1000, add: 1000, menu: 1000 };

// Headless screencasts have no mouse pointer; draw one so viewers can follow the clicks.
// It lives in the top layer so dropdown flyouts don't cover it.
async function showPointer(page) {
  await page.evaluate(() => {
    const pointer = document.createElement("div");
    pointer.popover = "manual";
    pointer.style.cssText =
      "inset:auto;margin:0;padding:0;border:0;background:none;overflow:visible;pointer-events:none;width:20px;height:24px;left:-40px;top:-40px;position:fixed";
    pointer.innerHTML =
      '<svg width="20" height="24" viewBox="0 0 20 24"><path d="M2 2v18l5-5 3.5 7 3-1.5L10 13.5h7z" fill="#f0e6d2" stroke="#010a13" stroke-width="1.5" stroke-linejoin="round"/></svg>';
    document.body.append(pointer);
    const raise = () => {
      pointer.hidePopover();
      pointer.showPopover();
    };
    pointer.showPopover();
    document.addEventListener("mousemove", (event) => {
      pointer.style.left = `${event.clientX - 2}px`;
      pointer.style.top = `${event.clientY - 2}px`;
    }, true);
    document.addEventListener("click", () => setTimeout(raise, 0), true);
  });
  await page.mouse.move(700, 560);
}

async function clickAt(page, selector, { steps = 18, dwell = 350 } = {}) {
  const box = await (await page.waitForSelector(selector, { visible: true })).boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps });
  await sleep(dwell);
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
}

// Animated screens: scene -> scripted interaction while recording.
const CLIPS = {
  // The README hero: find a duo partner's elo, log in with one click, glance at TFT ranks.
  demo: async (page) => {
    await showPointer(page);
    await sleep(1400);
    await clickAt(page, "#friend-tier + .framed-dropdown .framed-dropdown-current");
    await sleep(700);
    const gold = await page.$$eval("#friend-tier option", (options) => options.findIndex((option) => option.value === "Gold"));
    await clickAt(page, `#friend-tier + .framed-dropdown .framed-dropdown-option[data-index="${gold}"]`, { steps: 14 });
    await sleep(2000);
    await clickAt(page, "#accounts > .card:first-child .btn-play", { steps: 24, dwell: 500 });
    await page.waitForSelector('.login-overlay [data-login="close"]', { visible: true, timeout: 20000 });
    await sleep(1600);
    await clickAt(page, '.login-overlay [data-login="close"]');
    await page.waitForSelector(".login-overlay", { hidden: true });
    await sleep(600);
    await clickAt(page, '.mode-btn[data-mode="tft"]', { steps: 24 });
    await sleep(2600);
  },
};

const requested = new Set(process.argv.slice(2));
for (const scene of requested) {
  if (!(scene in STILLS) && !(scene in CLIPS)) throw new Error(`Unknown capture scene: ${scene}`);
}

function toGif(input, output) {
  const filter =
    "fps=12,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=160:stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a:diff_mode=rectangle";
  execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-i", input, "-vf", filter, "-loop", "0", output]);
}

mkdirSync(OUT, { recursive: true });
mkdirSync(`${OUT}/.tmp`, { recursive: true });
const demo = await demoBrowser();
const { browser, base } = demo;

try {
  for (const [scene, wait] of Object.entries(STILLS)) {
    if (requested.size && !requested.has(scene)) continue;
    const page = await capturePage(browser, base, scene);
    await sleep(wait);
    await page.screenshot({ path: `${OUT}/.tmp/${scene}.png` });
    renameSync(`${OUT}/.tmp/${scene}.png`, `${OUT}/${scene}.png`);
    await page.close();
    console.log(`still ${scene}`);
  }
  for (const [scene, act] of Object.entries(CLIPS)) {
    if (requested.size && !requested.has(scene)) continue;
    const page = await capturePage(browser, base);
    const recorder = await page.screencast({ path: `${OUT}/.tmp/${scene}.webm` });
    try {
      await act(page);
    } finally {
      await recorder.stop();
    }
    // Let in-flight CDP frame acknowledgements settle before closing the target.
    await sleep(250);
    await page.close();
    toGif(`${OUT}/.tmp/${scene}.webm`, `${OUT}/.tmp/${scene}.gif`);
    renameSync(`${OUT}/.tmp/${scene}.gif`, `${OUT}/${scene}.gif`);
    console.log(`gif ${scene}`);
  }
} finally {
  await demo.close();
  // This is the fixed, resolved scratch directory created above.
  const scratch = resolve(OUT, ".tmp");
  if (!scratch.startsWith(`${OUT}\\`) && !scratch.startsWith(`${OUT}/`)) throw new Error("Invalid capture scratch path");
  rmSync(scratch, { recursive: true, force: true });
}
