import test from "node:test";
import assert from "node:assert/strict";
import { demoBrowser, demoPage } from "../scripts/demo-browser.mjs";

// The browser mock simulates the backend: `?scene=updating` finds an update at
// launch, `?scene=restart` finishes one in the background.
test("automatic updates: launch screen, idle restart and download fallback", { timeout: 120000 }, async t => {
  const demo = await demoBrowser();
  const errors = [];
  const watch = page => {
    page.on("pageerror", error => errors.push(error.message));
    // Watched from navigation on, so the browser's own favicon request is seen too.
    page.on("response", response => {
      if (response.status() >= 400 && !response.url().endsWith("/favicon.ico")) errors.push(`${response.status()} ${response.url()}`);
    });
    return page;
  };
  async function launch(scene, lateStartup = false) {
    const page = watch(await demo.browser.newPage());
    await page.setViewport({ width: 1360, height: 860, deviceScaleFactor: 1 });
    await page.evaluateOnNewDocument((late) => {
      localStorage.clear();
      localStorage.setItem("leagueaccounts.dev.lang", "en");
      // Deterministically model a cold module load that already consumed the
      // three-second launch-check hold. The scene must still be downloading
      // when bootstrap reads the backend, rather than reveal a normal app.
      if (late) {
        const now = performance.now.bind(performance);
        performance.now = () => now() + 4000;
      }
    }, lateStartup);
    await page.goto(`${demo.base}?scene=${scene}`, { waitUntil: "domcontentloaded" });
    return page;
  }
  const progress = page => page.$eval(".update-meter", el => Number(el.getAttribute("aria-valuenow")));
  try {
    await t.test("an update found at launch replaces the app with the update screen, then restarts", async () => {
      const page = await launch("updating", true);
      try {
        await page.waitForSelector("#startup-loader .update-screen");
        assert.match(await page.$eval(".update-screen-title", el => el.textContent), /Updating League Accounts/i);
        assert.equal(await page.$eval(".update-screen-title", el => getComputedStyle(el).textTransform), "uppercase");
        assert.equal(await page.$("body.ready"), null);
        assert.equal(await page.$eval(".titlebar", el => getComputedStyle(el).visibility), "hidden");
        const first = await progress(page);
        await page.waitForFunction(start => Number(document.querySelector(".update-meter").getAttribute("aria-valuenow")) > start, {}, first);
        assert.match(await page.$eval(".update-screen-detail", el => el.textContent), /^\d+\.\d \/ 14\.0 MB$/);
        // Installed: the app restarts by itself, here as a reload without the scene.
        await page.waitForNavigation({ timeout: 20000 });
        assert.equal(new URL(page.url()).search, "");
        await page.waitForSelector("body.ready #accounts > .card");
        assert.equal(await page.$(".update-screen"), null);
      } finally {
        await page.close();
      }
    });

    await t.test("continuing in the background shows the app and asks before restarting", async () => {
      const page = await launch("updating");
      try {
        await page.waitForSelector(".update-screen button");
        await page.click(".update-screen button");
        await page.waitForSelector("body.ready #accounts > .card");
        assert.equal(await page.$("#startup-loader"), null);
        await page.waitForSelector("[data-update-notice]", { timeout: 15000 });
        assert.match(await page.$eval("[data-update-notice]", el => el.textContent), /Update installed[\s\S]*Restarting to finish in \d+ s\./);
        await page.click('[data-update-notice] [data-update="later"]');
        await page.waitForSelector("[data-update-notice]", { hidden: true });
        assert.match(await page.$eval(".toast", el => el.textContent), /Version 3\.1\.0 starts the next time you open League Accounts/);
        await new Promise(resolve => setTimeout(resolve, 1500));
        assert.equal(new URL(page.url()).searchParams.get("scene"), "updating");
      } finally {
        await page.close();
      }
    });

    await t.test("the restart countdown stops while a dialog is open; Restart now restarts", async () => {
      const page = await launch("restart");
      try {
        await page.waitForSelector("body.ready #accounts > .card");
        await page.waitForSelector("[data-update-notice]", { timeout: 10000 });
        await page.click('.side-link[data-action="settings"]');
        await page.waitForSelector(".modal");
        await page.waitForSelector("[data-update-notice]", { hidden: true, timeout: 3000 });
        await page.keyboard.press("Escape");
        await page.waitForSelector(".modal", { hidden: true });
        // The next idle check shows the notice again.
        await page.waitForSelector("[data-update-notice]", { timeout: 20000 });
        await Promise.all([
          page.waitForNavigation({ timeout: 5000 }),
          page.click('[data-update-notice] [data-update="restart"]'),
        ]);
        assert.equal(new URL(page.url()).search, "");
      } finally {
        await page.close();
      }
    });

    await t.test("Settings › Check now installs, or only notifies when automatic updates are off", async () => {
      const page = watch(await demoPage(demo.browser, demo.base, "settings"));
      try {
        await page.waitForSelector('.modal [name="autoUpdate"]');
        const row = await page.$eval('.modal [name="autoUpdate"]', el => ({ checked: el.checked, text: el.closest(".toggle-row").textContent }));
        assert.equal(row.checked, true);
        assert.match(row.text, /Update automatically[\s\S]*Installs new versions in the background[\s\S]*checks only when you click Check now/);
        await page.click(".modal [data-check-update]");
        await page.waitForFunction(() => /Downloading version 3\.1\.0/.test(document.querySelector(".update-result").textContent));
        await page.waitForFunction(() => /Version 3\.1\.0 is installed/.test(document.querySelector(".update-result").textContent), { timeout: 15000 });
        // Settings is open: no restart notice yet.
        assert.equal(await page.$("[data-update-notice]"), null);
      } finally {
        await page.close();
      }
      const manual = watch(await demoPage(demo.browser, demo.base, "settings"));
      try {
        await manual.waitForSelector('.modal [name="autoUpdate"]');
        await manual.click('.modal [name="autoUpdate"] + .switch');
        await manual.click('.modal [data-value="save"]');
        await manual.waitForSelector(".modal", { hidden: true });
        await manual.click('.side-link[data-action="settings"]');
        await manual.waitForSelector('.modal [name="autoUpdate"]');
        assert.equal(await manual.$eval('.modal [name="autoUpdate"]', el => el.checked), false);
        await manual.click(".modal [data-check-update]");
        await manual.waitForFunction(() => /Version 3\.1\.0 is ready to download/.test(document.querySelector(".update-result").textContent));
        assert.match(await manual.$eval(".update-banner", el => el.textContent), /Update available[\s\S]*Download/);
      } finally {
        await manual.close();
      }
    });

    assert.deepEqual(errors, []);
  } finally {
    await demo.close();
  }
});
