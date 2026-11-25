// Headless browser smoke test of a review page.
//
// Usage: node ci/smoke.mjs <page.html> <out-dir>
//
// Opens the page in Chromium once with a light and once with a dark color
// scheme, visits every view and opens an item on the canvas. Fails on any
// uncaught error, console error or failed request, on an empty view, and if
// both themes render with the same background. Screenshots of every view go
// to <out-dir>.
//
// Every state is also checked with axe-core's color-contrast rule (WCAG 2 AA:
// 4.5:1 for normal text, 3:1 for large text). Needs `axe-core` installed
// next to `playwright`.

import { chromium } from "playwright";
import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const axe_path = createRequire(import.meta.url).resolve("axe-core/axe.min.js");

const [page_path, out_dir] = process.argv.slice(2);
if (!page_path || !out_dir) {
  console.error("usage: node ci/smoke.mjs <page.html> <out-dir>");
  process.exit(2);
}
mkdirSync(out_dir, { recursive: true });
const url = pathToFileURL(resolve(page_path)).href;

const VIEWS = ["overview", "module", "canvas", "calls", "space"];
const failures = [];
// Contrast violations, keyed by theme and color pair, so that a pair that
// shows on many elements is reported once.
const contrast = new Map();

// Runs axe's color-contrast rule on the page as it is now shown.
async function check_contrast(page, scheme, where) {
  await page.addScriptTag({ path: axe_path });
  const nodes = await page.evaluate(async () => {
    const r = await window.axe.run(document, {
      runOnly: { type: "rule", values: ["color-contrast"] },
      resultTypes: ["violations"],
    });
    return r.violations.flatMap((v) =>
      v.nodes.map((n) => ({
        html: n.html.slice(0, 120),
        data: n.any[0]?.data ?? {},
      })),
    );
  });
  for (const n of nodes) {
    const d = n.data;
    const key = `[${scheme}] ${d.fgColor} on ${d.bgColor}`;
    const seen = contrast.get(key);
    if (seen) {
      seen.count += 1;
    } else {
      contrast.set(key, {
        text: `${key}: ${d.contrastRatio}:1, needs ${d.expectedContrastRatio} (${where}, e.g. ${n.html})`,
        count: 1,
      });
    }
  }
}
const backgrounds = {};

const browser = await chromium.launch();
try {
  for (const scheme of ["light", "dark"]) {
    const context = await browser.newContext({
      colorScheme: scheme,
      viewport: { width: 1400, height: 900 },
    });
    const page = await context.newPage();
    const fail = (msg) => failures.push(`[${scheme}] ${msg}`);
    page.on("pageerror", (e) => fail(`uncaught error: ${e.message}`));
    page.on("console", (m) => {
      if (m.type() === "error") fail(`console error: ${m.text()}`);
    });
    page.on("requestfailed", (r) => fail(`request failed: ${r.url()}`));

    await page.goto(url);
    // The viewer fills the sidebar once the wasm module has started.
    await page
      .waitForFunction(() => document.querySelector("#sidebar")?.children.length > 0, null, {
        timeout: 30_000,
      })
      .catch(() => fail("the viewer did not start (sidebar stays empty)"));

    if (!(await page.evaluate(() => document.body.classList.contains("reviewing")))) {
      fail("review mode is not on when the page opens");
    }

    backgrounds[scheme] = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);

    for (const view of VIEWS) {
      await page.click(`#views [data-view="${view}"]`);
      const section = page.locator(`#view-${view}`);
      try {
        await section.waitFor({ state: "visible", timeout: 5_000 });
      } catch {
        fail(`view "${view}" is not shown after clicking its tab`);
        continue;
      }
      const empty = await section.evaluate((el) => el.innerText.trim() === "" && el.querySelector("svg, canvas, .card") === null);
      if (empty) fail(`view "${view}" is empty`);
      await page.screenshot({ path: `${out_dir}/${scheme}-${view}.png` });
      await check_contrast(page, scheme, `${view} view`);
    }

    // Open an item of the overview's changed items on the canvas (a link
    // in a call graph's summary scrolls to the item on the overview).
    await page.click('#views [data-view="overview"]');
    const items = "#view-overview .cgi .irow > .iacts [data-open]";
    const open = page.locator(items).first();
    if ((await open.count()) === 0) {
      fail("the overview has no item to open");
    } else {
      await open.click();
      try {
        await page.locator("#cards .card").first().waitFor({ state: "visible", timeout: 5_000 });
        await page.screenshot({ path: `${out_dir}/${scheme}-canvas-card.png` });
        await check_contrast(page, scheme, "canvas card");
        // Each listed item alone on the canvas, with its body diff
        // expanded, so that changed words in source diffs are checked too.
        // One at a time: axe skips text that another card covers.
        const ids = [...new Set(await page.$$eval(items, (els) => els.map((e) => e.dataset.open)))];
        for (const id of ids) {
          await page.click('#views [data-view="canvas"]');
          await page.click("#cv-clear");
          await page.click('#views [data-view="overview"]');
          await page.locator(`${items}[data-open="${id}"]`).first().click();
          await page.locator("#cards .card").first().waitFor({ state: "visible", timeout: 5_000 });
          await page.$$eval("#cards details.src:not([open])", (ds) => ds.forEach((d) => (d.open = true)));
          await check_contrast(page, scheme, `canvas card of ${id} with its body diff`);
          // Cards are as wide as their code (up to a limit), so that it
          // needs no sideways scrolling.
          const cut = await page.$$eval("#cards .card", (cards) =>
            cards.flatMap((c) =>
              c.offsetWidth >= 1000
                ? []
                : [...c.querySelectorAll("pre, .sig, .sbs")]
                    .filter((b) => b.scrollWidth > b.clientWidth)
                    .map((b) => `${b.scrollWidth - b.clientWidth}px in a ${c.offsetWidth}px card`),
            ),
          );
          if (cut.length > 0) fail(`[${scheme}] canvas card of ${id} cuts off code: ${cut.join(", ")}`);
        }
      } catch (e) {
        fail(`opening an item shows no card on the canvas: ${e.message.split("\n")[0]}`);
      }
    }

    // Approving a changed file folds it to its header row, also when its
    // diff is split into parts (the demo's YAML workflow), and Expand shows
    // it again. Undone afterwards, so the checks below see no marks.
    await page.click('#views [data-view="overview"]');
    // Picked by position: once approved, a file no longer matches `.rvc.open`.
    const entries = page.locator("#view-overview .ientry.file");
    const unreviewed = await entries.evaluateAll((els) =>
      els.flatMap((el, i) => (el.querySelector(":scope > .irow:not(.part) .rvc.open") ? [i] : [])),
    );
    if (unreviewed.length === 0) fail("the overview has no changed file to approve");
    for (const i of unreviewed) {
      const file = entries.nth(i);
      const name = (await file.locator(":scope > .irow .title-name").first().textContent())?.trim();
      // The rows below the header, shown or not.
      const shown = () =>
        file.evaluate((el) =>
          [...el.children].slice(1).filter((c) => c.getClientRects().length > 0).length,
        );
      const all = await file.evaluate((el) => el.children.length - 1);
      await file.locator(":scope > .irow .rv-btn.approve").first().click();
      if ((await shown()) !== 0) fail(`approving ${name} leaves ${await shown()} of its ${all} rows shown`);
      await file.locator(":scope > .irow .rv-fold").first().click();
      if ((await shown()) !== all) fail(`Expand on ${name} shows ${await shown()} of its ${all} rows`);
      await file.locator(":scope > .irow [data-rv-undo]").first().click();
    }

    // The review keys: `j` outlines the next unreviewed item, `?` shows
    // the list of shortcuts and hides it again.
    await page.click('#views [data-view="overview"]');
    await page.locator("body").press("j");
    await page
      .locator(".rv-cur")
      .first()
      .waitFor({ state: "visible", timeout: 5_000 })
      .catch(() => fail("`j` outlines no current item"));
    await page.keyboard.press("?");
    try {
      await page.locator("#modal-b .keys").waitFor({ state: "visible", timeout: 5_000 });
      await page.screenshot({ path: `${out_dir}/${scheme}-keys.png` });
      await check_contrast(page, scheme, "keyboard shortcuts");
      await page.keyboard.press("?");
      await page.locator("#modal").waitFor({ state: "hidden", timeout: 5_000 });
    } catch (e) {
      fail(`\`?\` doesn't show and hide the shortcuts: ${e.message.split("\n")[0]}`);
    }

    // Review mode is on by default, so the views above were checked with
    // the review bar and the verdict controls. Check the canvas once more
    // with it turned off.
    const exit = page.locator("#rv-exit");
    if ((await exit.count()) > 0 && (await exit.isVisible())) {
      await exit.click();
      await page.screenshot({ path: `${out_dir}/${scheme}-review-off.png` });
      await check_contrast(page, scheme, "review mode off");
    }

    // The theme in the settings overrides the system's, also after a
    // reload, until "System" is picked again. Checked once, with the system
    // in dark mode, after the light background is known.
    if (scheme === "dark") {
      const bg = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
      const pick = async (theme) => {
        if (!(await page.locator("#settings").isVisible())) await page.click("#settings-btn");
        await page.check(`#settings input[name="set-theme"][value="${theme}"]`);
      };
      await pick("light");
      if ((await bg()) !== backgrounds.light) fail("picking the light theme doesn't show it");
      await page.reload();
      await page.waitForFunction(() => document.querySelector("#sidebar")?.children.length > 0);
      if ((await bg()) !== backgrounds.light) fail("the light theme is not kept after a reload");
      await pick("system");
      if ((await bg()) !== backgrounds.dark) fail("picking the system theme doesn't show it again");
    }
    await context.close();
  }
} finally {
  await browser.close();
}

// A press on a code block's scroll bar is left to the browser: it must not
// start moving the view (it did, so the bar could not be dragged). Needs a
// browser that shows scroll bars, which headless Chromium hides by default.
const bars = await chromium.launch({ ignoreDefaultArgs: ["--hide-scrollbars"] });
try {
  const page = await bars.newPage({ viewport: { width: 1400, height: 1600 } });
  await page.goto(url);
  await page.waitForFunction(() => document.querySelector("#sidebar")?.children.length > 0, null, { timeout: 30_000 });
  await page.locator("#view-overview .cgi .irow > .iacts [data-open]").first().click();
  const card = page.locator("#cards .card").first();
  await card.waitFor({ state: "visible", timeout: 5_000 });
  await page.$$eval("#cards details.src:not([open])", (ds) => ds.forEach((d) => (d.open = true)));
  // Narrow enough that the code needs scrolling.
  await card.evaluate((c) => (c.style.width = "320px"));
  const bar = await card.evaluate((c) => {
    const b = [...c.querySelectorAll("pre, .sig, .sbs")].find(
      (b) => b.scrollWidth > b.clientWidth && b.offsetHeight > b.clientHeight,
    );
    if (!b) return null;
    const r = b.getBoundingClientRect();
    return { x: r.left + 40, y: (r.top + b.clientTop + b.clientHeight + r.bottom) / 2, bottom: innerHeight };
  });
  if (!bar || bar.y > bar.bottom) {
    failures.push("found no code block with a scroll bar in view to press");
  } else {
    await page.mouse.move(bar.x, bar.y);
    await page.mouse.down();
    const panning = await page.evaluate(() => document.getElementById("viewport").classList.contains("panning"));
    await page.mouse.up();
    if (panning) failures.push("a press on a code block's scroll bar moves the view instead of the bar");
  }
} finally {
  await bars.close();
}

if (backgrounds.light === backgrounds.dark) {
  failures.push(`light and dark themes have the same background (${backgrounds.light})`);
}
if (contrast.size > 0) {
  failures.push(
    `${contrast.size} text color pair(s) below the WCAG AA contrast ratio:\n${[...contrast.values()]
      .map((c) => `      ${c.text} (${c.count} element${c.count > 1 ? "s" : ""})`)
      .join("\n")}`,
  );
}
if (failures.length > 0) {
  console.error(`Smoke test failed:\n${failures.map((f) => `  - ${f}`).join("\n")}`);
  process.exit(1);
}
console.log(`Smoke test passed: ${VIEWS.length} views in light and dark, contrast checked (backgrounds ${backgrounds.light}, ${backgrounds.dark}).`);
