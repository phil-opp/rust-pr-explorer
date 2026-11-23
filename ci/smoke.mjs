// Headless browser smoke test of an explorer page.
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

    // Open an item from the overview on the canvas.
    await page.click('#views [data-view="overview"]');
    const open = page.locator("#view-overview [data-open]").first();
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
        const ids = await page.$$eval("#view-overview [data-open]", (els) => els.map((e) => e.dataset.open));
        for (const id of ids) {
          await page.click('#views [data-view="canvas"]');
          await page.click("#cv-clear");
          await page.click('#views [data-view="overview"]');
          await page.click(`#view-overview [data-open="${id}"]`);
          await page.locator("#cards .card").first().waitFor({ state: "visible", timeout: 5_000 });
          await page.$$eval("#cards details.src:not([open])", (ds) => ds.forEach((d) => (d.open = true)));
          await check_contrast(page, scheme, `canvas card of ${id} with its body diff`);
        }
      } catch (e) {
        fail(`opening an item shows no card on the canvas: ${e.message.split("\n")[0]}`);
      }
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
    await context.close();
  }
} finally {
  await browser.close();
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
