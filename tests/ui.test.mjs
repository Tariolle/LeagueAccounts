import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { demoBrowser, demoPage } from "../scripts/demo-browser.mjs";

test("account collection, client frames and silent motion", { timeout: 180000 }, async t => {
  const demo = await demoBrowser();
  const errors = [];
  let page;
  async function open(scene = "main", size) {
    await page?.close();
    page = await demoPage(demo.browser, demo.base, scene, size);
    page.on("pageerror", error => errors.push(error.message));
    page.on("response", response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`); });
    return page;
  }
  const text = selector => page.$eval(selector, el => el.textContent);
  const count = () => page.$$eval("#accounts > .card", elements => elements.length);
  async function noHorizontalOverflow(selector) {
    assert.equal(await page.$eval(selector, el => el.scrollWidth <= el.clientWidth + 1), true, `${selector} overflows horizontally`);
  }
  try {
    await t.test("ranked overlays fit full and short histories in both account layouts", async () => {
      await open("ranked-cards");
      assert.equal(await page.$$eval(".recent-game img", imgs => imgs.every(img => img.complete && img.naturalWidth > 0)), true);
      const rates = await page.$$eval(".ranked-stat-rate", els => Object.fromEntries(els.map(el => [el.textContent, el.className])));
      assert.match(rates["49%"], /is-negative/);
      assert.doesNotMatch(rates["50%"], /is-positive|is-negative/);
      assert.match(rates["51%"], /is-positive/);
      for (const width of [980, 1360, 1800]) {
        await page.setViewport({ width, height: 860 });
        for (const view of ["grid", "list"]) {
          await page.click(`[data-view="${view}"]`);
          await noHorizontalOverflow("#accounts");
          const layouts = await page.$$eval(".recent-games", rows => rows.map(row => {
            const r = row.getBoundingClientRect();
            const icons = [...row.children].map(icon => icon.getBoundingClientRect());
            const actions = row.closest(".card").querySelector(".card-actions").getBoundingClientRect();
            return { visible: r.width > 0, count: icons.length, square: icons.every(i => Math.abs(i.width - i.height) < 1),
              left: icons[0].left - r.left, right: r.right - icons.at(-1).right,
              gap: actions.top - r.bottom, overlapsActions: r.right > actions.left,
              overflow: row.scrollWidth > row.clientWidth + 1 };
          }));
          assert.deepEqual(layouts.map(row => row.count), [10, 2, 10]);
          for (const row of layouts.filter(row => row.visible)) {
            assert.equal(row.overflow, false, `${view} ${width}: history overflow`);
            assert.equal(row.square, true, `${view} ${width}: distorted portraits`);
            assert.ok(Math.abs(row.left) <= 1, `${view} ${width}: short histories start at the left`);
            if (row.count === 10) assert.ok(Math.abs(row.right) <= 1, `${view} ${width}: full histories fill the row`);
            if (view === "grid") assert.ok(row.gap >= 3 && row.gap <= 8, `button gap: ${row.gap}`);
            else assert.equal(row.overlapsActions, false, `${view} ${width}: portraits overlap actions`);
          }
        }
      }
      await page.click('.mode-btn[data-mode="tft"]');
      assert.equal(await page.$(".recent-games"), null);
      assert.equal(await page.$(".ranked-stats"), null);
      await page.click('.mode-btn[data-mode="lol"]');
      assert.equal(await page.$$eval(".recent-game", icons => icons.length), 22);
    });
    await t.test("browser dev data covers every rank in both games and resets edits", async () => {
      await open();
      assert.equal(await page.$eval("#dev-tools", el => el.hidden), false);
      const expected = ["Challenger", "Grandmaster", "Master", "Diamond", "Emerald", "Platinum", "Gold", "Silver", "Bronze", "Iron", "Unranked"];
      for (const mode of ["lol", "tft"]) {
        await page.click(`.mode-btn[data-mode="${mode}"]`);
        assert.equal(await count(), 11);
        const distribution = await page.$$eval(".dist-row", rows => rows.map(row => [row.dataset.tier, row.querySelector(".dist-count").textContent]));
        assert.deepEqual(distribution, expected.map(tier => [tier, "1"]));
      }
      await page.click('#accounts > .card:first-child [data-act="menu"]');
      await page.click('.menu-item[data-index="1"]');
      await page.waitForSelector('.modal [name="name"]');
      await page.$eval('.modal [name="name"]', el => { el.value = "Edited#DEV"; });
      await page.click('.modal [data-value="save"]');
      await page.waitForSelector('.modal', { hidden: true });
      assert.match(await text("#accounts"), /Edited/);
      await Promise.all([
        page.waitForNavigation({ waitUntil: "networkidle0" }),
        page.click("#reset-demo"),
      ]);
      await page.waitForSelector("body.ready #accounts > .card");
      assert.equal(await count(), 11);
      assert.doesNotMatch(await text("#accounts"), /Edited/);
      assert.equal(await page.$eval("html", el => el.dataset.mode), "lol");
    });
    await t.test("rank, region and search filters keep counts and reset in sync", async () => {
      await open();
      assert.equal(await count(), 11);
      assert.equal(await text("#account-count"), "11 of 11 accounts");
      await page.select("#region-filter", "EUW");
      assert.equal(await count(), 3);
      await page.type("#search", "Hextech");
      assert.equal(await count(), 1);
      assert.equal(await text("#account-count"), "1 of 11 accounts");
      await page.click("#clear-filters");
      assert.equal(await count(), 11);
      await page.click('.dist-row[data-tier="Diamond"]');
      assert.equal(await count(), 1);
      await page.click("#clear-filters");
      try {
        await Promise.all([
          page.waitForSelector("#clear-filters", { hidden: true, timeout: 60000 }),
          page.waitForFunction(() => document.querySelectorAll("#accounts > .card").length === 11, { timeout: 60000 }),
        ]);
      } catch (error) {
        const diagnosticDir = join(tmpdir(), `leagueaccounts-clear-filters-${process.pid}`);
        await mkdir(diagnosticDir, { recursive: true }).catch(() => {});
        const screenshotPath = join(diagnosticDir, "failure.png");
        const htmlPath = join(diagnosticDir, "page.html");
        await page.screenshot({ path: screenshotPath, fullPage: true }).catch(() => {});
        const html = await page.content().catch(() => "<failed to capture html>");
        await writeFile(htmlPath, html).catch(() => {});
        console.error(`Clear-filters restore failed; diagnostics: ${screenshotPath}, ${htmlPath}`);
        console.error("Page HTML snapshot (first 20k chars):\n", html.slice(0, 20000));
        throw error;
      }
      assert.equal(await count(), 11);
      await page.select("#sort", "level");
      assert.match(await text("#accounts > .card:first-child"), /Lux Main/);
      await page.click("#accounts > .card:first-child .identity");
      await page.keyboard.press("ArrowRight");
      assert.match(await text("#accounts > .selected"), /Hextech Fox/);
    });
    await t.test("game modes use their respective client crests; compact view fits minimum size", async () => {
      await open();
      await page.click('.mode-btn[data-mode="tft"]');
      assert.match(await text("#queue-label"), /Teamfight Tactics/i);
      await page.waitForFunction(() => [...document.querySelectorAll("#accounts .emblem img")].every(img => img.complete && img.naturalWidth > 0));
      // The client renders RANKED_TFT with the same full emblems; mini crests stay in the sidebar.
      assert.equal(await page.$$eval("#accounts .emblem img", imgs => imgs.every(img => /\/ranks\/\w+\.png$/.test(img.src) && img.naturalWidth > 0)), true);
      assert.equal(await page.$$eval(".dist-emblem img", imgs => imgs.length > 0 && imgs.every(img => img.src.endsWith("-tft.svg") || img.src.endsWith("unranked-tft.svg") || img.src.endsWith("/crests/unranked.svg"))), true);
      await page.click('[data-view="list"]');
      await page.setViewport({ width: 980, height: 640 });
      await noHorizontalOverflow(".main");
      await noHorizontalOverflow("#accounts");
      await page.click('.mode-btn[data-mode="lol"]');
      assert.match(await text("#queue-label"), /Solo/);
      assert.equal(await count(), 11);
    });
    await t.test("selected game tabs visibly respond to keyboard focus with either motion preference", async () => {
      await open();
      const appearance = selector => page.$eval(selector, el => ({
        focused: el.matches(":focus-visible"),
        selected: el.getAttribute("aria-selected"),
        outline: getComputedStyle(el).outlineStyle,
        paint: {
          background: getComputedStyle(el).backgroundImage,
          navigationLight: getComputedStyle(el, "::after").opacity,
        },
      }));
      for (const motion of ["no-preference", "reduce"]) {
        await page.emulateMediaFeatures([{ name: "prefers-reduced-motion", value: motion }]);
        for (const mode of ["lol", "tft"]) {
          const selector = `.mode-btn[data-mode="${mode}"]`;
          await page.click(selector);
          await page.mouse.move(0, 90);
          await page.keyboard.press("Tab");
          const resting = await appearance(selector);
          assert.equal(resting.focused, false);
          await page.keyboard.down("Shift");
          await page.keyboard.press("Tab");
          await page.keyboard.up("Shift");
          const focused = await appearance(selector);
          assert.equal(focused.focused, true);
          assert.equal(focused.selected, "true", "focusing a tab must preserve selection");
          assert.equal(focused.outline, "none", "use the client's gold navigation light, not a generic outline");
          assert.notDeepEqual(focused.paint, resting.paint, `${mode} focus must stay visible with ${motion}`);
        }
      }
    });
    await t.test("Settings frame contains scrolling content while its actions and close stay reachable", async () => {
      // Short enough that the two-column body still has to scroll.
      await open("settings", { width: 980, height: 480 });
      await page.waitForSelector(".modal");
      assert.equal(await page.$eval(".modal-body", el => el.scrollHeight > el.clientHeight), true);
      const bounds = await page.$eval(".modal", el => {
        const r = el.getBoundingClientRect();
        const c = el.querySelector(".modal-close").getBoundingClientRect();
        const f = el.querySelector(".modal-foot").getBoundingClientRect();
        return r.left >= 0 && r.right <= innerWidth && c.top >= 64 && c.right <= innerWidth && f.bottom <= innerHeight;
      });
      assert.equal(bounds, true);
      await noHorizontalOverflow(".modal-body");
      await page.focus('.modal [name="launchAtStartup"]');
      await page.keyboard.press("Space");
      assert.equal(await page.$eval('[name="launchAtStartup"]', el => el.checked), true);
      await page.focus('.modal [name="loginMethod"][value="previous"]');
      await page.keyboard.press("Space");
      assert.equal(await page.$eval('[name="launchGame"]', el => el.disabled), true);
      await page.focus('.modal [data-value="save"]');
      await page.keyboard.press("Tab");
      assert.equal(await page.$eval(".modal-close", el => el === document.activeElement), true);
      await page.keyboard.down("Shift");
      await page.keyboard.press("Tab");
      await page.keyboard.up("Shift");
      assert.equal(await page.$eval('[data-value="save"]', el => el === document.activeElement), true);
      assert.equal(await page.$eval('.modal [name="uiScale"]', el => el.value), "100");
      await page.select('.modal [name="language"]', "fr");
      await page.select('.modal [name="uiScale"]', "125");
      await page.click('.modal [data-value="save"]');
      await page.waitForSelector(".modal", { hidden: true });
      assert.equal(await text("#account-count"), "11 comptes sur 11");
      await page.click('[data-action="settings"]');
      await page.waitForSelector('.modal [name="uiScale"]');
      assert.equal(await page.$eval('.modal [name="uiScale"]', el => el.value), "125");
      // Formatted for the language: "125 %" in French.
      assert.match(await page.$eval('.modal [name="uiScale"]', el => el.selectedOptions[0].textContent), /^125\s%$/);
    });
    await t.test("client tooltips support focus, escaping, dismissal and removed owners", async () => {
      await open();
      const selector = '#accounts > .card:first-child [data-act="copy-pass"]';
      await page.focus(selector);
      await page.waitForSelector('[role="tooltip"]');
      assert.equal(await text('[role="tooltip"]'), "Copy password");
      assert.equal(await page.$eval(selector, el => el.getAttribute("aria-describedby")), "client-tooltip");
      await page.keyboard.press("Escape");
      assert.equal(await page.$('[role="tooltip"]'), null);
      assert.equal(await page.$eval(selector, el => el.hasAttribute("aria-describedby")), false);
      await page.$eval(selector, el => { el.dataset.tooltip = '<img src=x onerror="alert(1)">'; });
      await page.hover(selector);
      await page.waitForSelector('[role="tooltip"]');
      assert.equal(await page.$eval('[role="tooltip"]', el => el.children.length), 0);
      const bounds = await page.$eval('[role="tooltip"]', el => { const r = el.getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth && r.top >= 0 && r.bottom <= innerHeight; });
      assert.equal(bounds, true);
      await page.select("#region-filter", "EUW");
      await page.waitForSelector('[role="tooltip"]', { hidden: true });
    });
    await t.test("adding an account without a Riot ID retains one-click login and the edit action", async () => {
      await open("add");
      await page.waitForSelector(".drawer");
      await page.type('.drawer [name="accountId"]', "ui_test_only");
      await page.type('.drawer [name="password"]', "synthetic-test-password");
      await page.click('.drawer [data-panel="single"] button[type="submit"]');
      await page.waitForFunction(() => document.querySelectorAll("#accounts > .card").length === 12);
      await page.click(".drawer [data-close]");
      await page.waitForSelector(".drawer", { hidden: true });
      await page.type("#search", "ui_test_only");
      assert.equal(await count(), 1);
      assert.match(await text("#accounts"), /No Riot ID associated/);
      assert.ok(await page.$("#accounts .rank-edit"));
      assert.equal(await page.$eval("#accounts .btn-play", el => el.disabled), false);
    });
    await t.test("rank crests stay static on hover and focus; login respects reduced motion", async () => {
      await open();
      await page.hover("#accounts > .card:nth-child(3)");
      await page.focus("#accounts > .card:nth-child(3)");
      assert.equal(await page.$$eval("video,audio,[data-crest-motion]", elements => elements.length), 0);
      await page.emulateMediaFeatures([{ name: "prefers-reduced-motion", value: "reduce" }]);
      await page.hover("#accounts > .card:first-child");
      assert.equal(await page.$$eval("video,audio", els => els.length), 0);
      await page.click("#accounts > .card:first-child .btn-play");
      await page.waitForSelector(".login-overlay");
      assert.equal(await page.$$eval("video,audio", els => els.length), 0);
      assert.equal(await page.$(".is-client-click"), null);
    });
    await t.test("login progress follows completed steps without video or audio", async () => {
      await open("login");
      await page.waitForSelector(".login-overlay");
      assert.equal(await page.$$eval("video,audio", els => els.length), 0);
      await page.waitForFunction(() => Number(document.querySelector('[role="progressbar"]').getAttribute("aria-valuenow")) > 0);
      const progress = await page.$eval('[role="progressbar"]', el => ({
        value: Number(el.getAttribute("aria-valuenow")),
        max: Number(el.getAttribute("aria-valuemax")),
        resolved: document.querySelectorAll(".login-steps .done").length,
        current: el.getAttribute("aria-valuetext"),
        label: document.querySelector(".login-steps .active .step-label").textContent,
      }));
      assert.equal(progress.value, progress.resolved);
      assert.ok(progress.value < progress.max);
      assert.equal(progress.current, progress.label);
      await page.waitForSelector(".login-overlay.is-success", { timeout: 15000 });
      assert.equal(await page.$$eval("video,audio", els => els.length), 0);
      assert.equal(await page.$eval('[role="progressbar"]', el => el.getAttribute("aria-valuenow") === el.getAttribute("aria-valuemax")), true);
      await page.click('[data-login="close"]');
      await page.waitForSelector(".login-overlay", { hidden: true });
    });
    await t.test("logging in anyway to a banned account launches the game", async () => {
      await open("banned");
      await page.waitForSelector('.modal [data-value="login"]');
      await page.click('.modal [data-value="login"]');
      await page.waitForSelector(".login-overlay.is-success", { timeout: 15000 });
      await page.click('[data-login="close"]');
      await page.waitForSelector(".login-overlay", { hidden: true });
    });
    await t.test("framed dropdowns open flush under their frame, follow the keyboard and keep Escape", async () => {
      await open();
      const flyout = select => page.$eval(`${select} + .framed-dropdown .framed-dropdown-options`, (list, owner) => {
        const frame = document.querySelector(owner).closest(".select-wrap").getBoundingClientRect();
        const box = list.getBoundingClientRect();
        return { open: list.matches(":popover-open"), top: Math.round(box.top), width: Math.round(box.width), frameBottom: Math.round(frame.bottom), frameWidth: Math.round(frame.width) };
      }, select);
      await page.click("#sort + .framed-dropdown .framed-dropdown-current");
      const sort = await flyout("#sort");
      assert.equal(sort.open, true);
      assert.equal(sort.top, sort.frameBottom);
      assert.equal(sort.width, sort.frameWidth);
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("Enter");
      assert.equal((await flyout("#sort")).open, false);
      assert.equal(await page.$eval("#sort", el => el.value), "name");
      assert.equal(await text("#sort + .framed-dropdown .framed-dropdown-current"), "Name");
      // The frame's own label opens its first dropdown; the division appears with a tier that has one.
      assert.equal(await page.$eval("#friend-division + .framed-dropdown", el => getComputedStyle(el).display), "none");
      await page.click("#friend-wrap .select-label");
      assert.equal((await flyout("#friend-tier")).open, true);
      await page.keyboard.type("dia");
      await page.keyboard.press("Enter");
      assert.equal(await page.$eval("#friend-tier", el => el.value), "Diamond");
      assert.notEqual(await page.$eval("#friend-division + .framed-dropdown", el => getComputedStyle(el).display), "none");
      // A clicked option closes the flyout; the frame's label handler must not reopen it.
      await page.click("#friend-tier + .framed-dropdown .framed-dropdown-current");
      const gold = await page.$$eval("#friend-tier option", options => options.findIndex(option => option.value === "Gold"));
      await page.click(`#friend-tier + .framed-dropdown .framed-dropdown-option[data-index="${gold}"]`);
      assert.equal(await page.$eval("#friend-tier", el => el.value), "Gold");
      assert.equal((await flyout("#friend-tier")).open, false);
      await page.click('[data-action="settings"]');
      await page.waitForSelector(".modal .framed-dropdown-current");
      await page.click(".modal .framed-dropdown-current");
      await page.waitForSelector(".modal .framed-dropdown-options:popover-open");
      await page.keyboard.press("Escape");
      assert.equal(await page.$(".framed-dropdown-options:popover-open"), null);
      assert.notEqual(await page.$(".modal"), null);
      await page.keyboard.press("Escape");
      await page.waitForSelector(".modal", { hidden: true });
    });
    await t.test("long tier names fit the narrowest cards", async () => {
      await open("main", { width: 1165, height: 760 });
      // German has the longest single-word tier name ("HERAUSFORDERER"); switch through Settings.
      await page.click('[data-action="settings"]');
      await page.waitForSelector(".modal .framed-dropdown-current");
      await page.click(".modal .framed-dropdown-current");
      await page.keyboard.type("deu");
      await page.keyboard.press("Enter");
      await page.click('.modal [data-value="save"]');
      await page.waitForSelector(".modal", { hidden: true });
      await page.waitForFunction(() => [...document.querySelectorAll("#accounts .rank-name")].some(name => name.textContent.includes("Herausforderer")));
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const names = await page.$$eval("#accounts .rank-name", els => els.map(el => ({ text: el.textContent, fits: el.scrollWidth <= el.clientWidth, size: parseFloat(getComputedStyle(el).fontSize) })));
      for (const name of names) {
        assert.ok(name.fits, `${name.text} overflows its column`);
        assert.ok(name.size >= 13, `${name.text} shrank to ${name.size}px`);
      }
      assert.ok(names.some(name => name.size < 20), "the narrowest cards should need to shrink a long name");
    });
    await t.test("filtering keeps the layout still; the header countdown refreshes silently", async () => {
      await open();
      const layout = () => page.evaluate(() => {
        const box = selector => { const r = document.querySelector(selector).getBoundingClientRect(); return [r.x, r.y, r.width, r.height].map(Math.round); };
        return { header: box(".collection-header"), toolbar: box(".toolbar"), card: box("#accounts > .card"), gridWidth: box("#accounts")[2], countRight: Math.round(document.querySelector("#account-count").getBoundingClientRect().right) };
      });
      const before = await layout();
      assert.equal(await page.$eval(".main", el => el.scrollHeight > el.clientHeight), true);
      await page.type("#search", "Piltover");
      assert.equal(await count(), 1);
      assert.equal(await page.$eval(".main", el => el.scrollHeight > el.clientHeight), false);
      assert.deepEqual(await layout(), before);
      await page.click("#clear-filters");
      assert.deepEqual(await layout(), before);
      assert.match(await text("#schedule"), /Next rank update:\s*\d+:\d{2}/i);
      await page.click("#schedule");
      await page.waitForSelector("#schedule.running");
      await page.waitForSelector("#schedule:not(.running)", { timeout: 60000 });
      assert.equal(await page.$(".toast"), null);
    });
    assert.deepEqual(errors, []);
  } finally {
    await demo.close();
  }
});
