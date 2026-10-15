import test from "node:test";
import assert from "node:assert/strict";
import { demoBrowser } from "../scripts/demo-browser.mjs";

test("startup waits for critical assets but not lazy account images", { timeout: 90000 }, async t => {
  const demo = await demoBrowser();
  try {
    await t.test("cold loads wait for app code, fonts, crests and CSS backgrounds", async () => {
      const page = await demo.browser.newPage();
      await page.emulateMediaFeatures([{ name: "prefers-reduced-motion", value: "no-preference" }]);
      await page.setCacheEnabled(false);
      await page.setRequestInterception(true);
      const blocked = new Set(["module", "font", "crest", "background"]);
      const pending = new Map([...blocked].map(type => [type, []]));
      page.on("request", request => {
        const path = new URL(request.url()).pathname;
        const type = path === "/src/main.ts" ? "module"
          : path.includes("/league/fonts/") ? "font"
          : path === "/league/ranks/challenger.png" ? "crest"
          : path === "/league/backgrounds/hextech.jpg" ? "background" : null;
        if (blocked.has(type)) pending.get(type).push(request);
        else void request.continue();
      });
      const release = async type => {
        blocked.delete(type);
        await Promise.all(pending.get(type).map(request => request.continue()));
      };
      const assertLoading = async () => {
        assert.deepEqual(await page.evaluate(() => ({
          ready: document.body.classList.contains("ready"),
          header: getComputedStyle(document.querySelector(".titlebar")).visibility,
          accounts: getComputedStyle(document.querySelector(".shell")).visibility,
          loader: getComputedStyle(document.querySelector("#startup-loader")).backgroundColor,
          busy: document.body.getAttribute("aria-busy"),
        })), { ready: false, header: "hidden", accounts: "hidden", loader: "rgb(6, 14, 23)", busy: "true" });
      };
      try {
        await page.evaluateOnNewDocument(() => {
          const decode = HTMLImageElement.prototype.decode;
          window.__lazyDecodeCalls = 0;
          HTMLImageElement.prototype.decode = function() {
            if (this.loading === "lazy") {
              window.__lazyDecodeCalls++;
              return new Promise(() => {});
            }
            return decode.call(this);
          };
        });
        await page.goto(`${demo.base}?scene=startup-lazy`, { waitUntil: "domcontentloaded" });
        await assertLoading();
        assert.equal(await page.$eval(".brand-icon", el => el.getBoundingClientRect().width), 28);
        // The client's hextech loader (Lottie) plays while the app module is held back.
        // It holds some poses, so watch up to one 70-frame loop (2.3 s) for movement.
        await page.waitForSelector("#startup-animation svg");
        await page.waitForFunction(() => {
          const svg = document.querySelector("#startup-animation svg").innerHTML;
          const moved = window.__firstFrame !== undefined && svg !== window.__firstFrame;
          window.__firstFrame ??= svg;
          return moved;
        }, { polling: 100, timeout: 2500 });
        await release("module");
        await page.waitForSelector("#accounts > .card");
        assert.equal(await page.$$eval(".recent-game img[loading=lazy]", imgs => imgs.length), 10);
        assert.equal(await page.$eval("#accounts > .card:has(.recent-games) .ranked-stats", el => el.textContent.trim()), "47W 33L · 59%");
        assert.equal(await page.$eval("#accounts > .card:has(.recent-games) .ranked-stat-rate", el => getComputedStyle(el).color), "rgb(50, 208, 88)");
        assert.equal(await page.$eval("#accounts > .card:has(.recent-games) .recent-game.is-win", el => getComputedStyle(el).borderTopColor), "rgb(50, 208, 88)");
        assert.equal(await page.$eval("#accounts > .card:has(.recent-games) .recent-game.is-loss", el => getComputedStyle(el).borderTopColor), "rgb(240, 68, 73)");
        await page.waitForFunction(() => document.fonts.status === "loading");
        await assertLoading();
        await release("font");
        await page.evaluate(() => document.fonts.ready.then(() => true));
        await assertLoading();
        await release("crest");
        await page.waitForFunction(() => [...document.images]
          .filter(img => img.loading !== "lazy")
          .every(img => img.complete && img.naturalWidth > 0));
        await assertLoading();
        await release("background");
        await page.waitForSelector("body.ready");
        assert.equal(await page.evaluate(() => window.__lazyDecodeCalls), 0);
        assert.equal(await page.$("#startup-loader"), null);
        assert.equal(await page.$eval(".titlebar", el => getComputedStyle(el).visibility), "visible");
        assert.equal(await page.$eval(".brand-icon", el => el.getBoundingClientRect().width), 28);
        assert.equal(await page.$$eval(".card.enter, .card.enter-stagger", elements => elements.length), 0);
        assert.equal(await page.$eval("body", el => el.getAttribute("aria-busy")), "false");
      } finally {
        await page.close();
      }
    });
    await t.test("the loading animation holds still under reduced motion", async () => {
      const page = await demo.browser.newPage();
      await page.emulateMediaFeatures([{ name: "prefers-reduced-motion", value: "reduce" }]);
      try {
        await page.goto(`${demo.base}?scene=loading`, { waitUntil: "domcontentloaded" });
        await page.waitForSelector("#startup-animation svg");
        const frame = () => page.$eval("#startup-animation svg", svg => svg.innerHTML);
        const first = await frame();
        await new Promise(resolve => setTimeout(resolve, 300));
        assert.equal(await frame(), first);
      } finally {
        await page.close();
      }
    });
    await t.test("failed app modules show a retry action without revealing a broken interface", async () => {
      const page = await demo.browser.newPage();
      await page.setCacheEnabled(false);
      await page.setRequestInterception(true);
      let fail = true;
      page.on("request", request => {
        if (fail && new URL(request.url()).pathname === "/src/main.ts") {
          fail = false;
          void request.abort("failed");
        } else void request.continue();
      });
      try {
        await page.goto(demo.base, { waitUntil: "domcontentloaded" });
        await page.waitForSelector("#startup-error", { visible: true });
        assert.equal(await page.$eval(".titlebar", el => getComputedStyle(el).visibility), "hidden");
        assert.equal(await page.$("body.ready"), null);
        await Promise.all([page.waitForNavigation(), page.click("#startup-retry")]);
        await page.waitForSelector("body.ready");
        assert.equal(await page.$("#startup-loader"), null);
        assert.equal(await page.$$eval("#accounts > .card", cards => cards.length), 11);
      } finally {
        await page.close();
      }
    });
  } finally {
    await demo.close();
  }
});
