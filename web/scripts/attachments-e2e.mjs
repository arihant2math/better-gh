#!/usr/bin/env node
// Attachments (P6) end-to-end against a REAL bgh-server serving web/dist:
// paste a screenshot into an issue comment, drop a disallowed file, reload
// and check the image renders; wiki and release-notes editors upload too.
// Seed: user ada/password123 with a private repo ada/demo and issue #1.
//
//   node web/scripts/attachments-e2e.mjs [baseUrl=http://127.0.0.1:3000] [shotsDir]
import { chromium } from "./lib/browser.mjs";
const base = process.argv[2] ?? "http://127.0.0.1:3000";
const shots = process.argv[3];
let failures = 0;
const check = (c, m) => {
  console.log(`${c ? "✓" : "✗"} ${m}`);
  if (!c) failures++;
};

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
page.on("pageerror", (e) => console.log("pageerror", e.message));
await page.goto(`${base}/`);
await page.getByLabel("Username or email address").fill("ada");
await page.getByLabel("Password").fill("password123");
await page.getByRole("button", { name: "Sign in" }).click();
await page.waitForSelector("[aria-label=Sidebar]", { timeout: 15000 });

await page.goto(`${base}/ada/demo/issues/1`);
const box = page.getByLabel("Comment body").last();
await box.waitFor();
await box.click();
await box.fill("Here is the bug:\n");

// A real PNG screenshot of the page, pasted through a ClipboardEvent.
const png = await page.screenshot({
  clip: { x: 0, y: 0, width: 200, height: 120 },
});
await box.evaluate((el, b64) => {
  const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  const dt = new DataTransfer();
  dt.items.add(new File([bytes], "image.png", { type: "image/png" }));
  el.dispatchEvent(
    new ClipboardEvent("paste", {
      clipboardData: dt,
      bubbles: true,
      cancelable: true,
    }),
  );
}, png.toString("base64"));
check(
  (await box.inputValue()).includes("![Uploading image.png…]()") ||
    (await box.inputValue()).includes("/user-attachments/assets/"),
  "placeholder inserted on paste",
);
await page.waitForFunction(
  () =>
    [...document.querySelectorAll("textarea")].some((t) =>
      /!\[image\.png\]\(http[^)]+\/user-attachments\/assets\/[0-9a-f-]{36}\)/.test(
        t.value,
      ),
    ),
  null,
  { timeout: 15000 },
);
const value = await box.inputValue();
check(
  !value.includes("Uploading"),
  "placeholder replaced with attachment markdown",
);
console.log("  ", value.replace(/\n/g, "⏎"));

// A disallowed file: placeholder removed, error toast.
await box.evaluate((el) => {
  const dt = new DataTransfer();
  dt.items.add(
    new File(["MZ"], "setup.exe", { type: "application/octet-stream" }),
  );
  el.dispatchEvent(
    new DragEvent("drop", {
      dataTransfer: dt,
      bubbles: true,
      cancelable: true,
    }),
  );
});
await page
  .getByRole("alert")
  .filter({ hasText: "Failed to upload setup.exe" })
  .waitFor({ timeout: 10000 });
check(
  !(await box.inputValue()).includes("setup.exe"),
  "failed upload removes its placeholder and toasts",
);

if (shots) await page.screenshot({ path: `${shots}/p6-editor.png` });
await box.focus();
await page.keyboard.press("Control+Enter");
await page.waitForTimeout(1500);
await page.reload();
const img = page
  .locator('.markdown-body img[src*="/user-attachments/assets/"]')
  .first();
await img.waitFor({ timeout: 15000 });
await page.waitForFunction(
  () => {
    const i = document.querySelector(
      '.markdown-body img[src*="/user-attachments/assets/"]',
    );
    return i && i.complete && i.naturalWidth > 0;
  },
  null,
  { timeout: 15000 },
);
check(true, "pasted screenshot renders in the comment after reload");
if (shots)
  await img
    .scrollIntoViewIfNeeded()
    .then(() => page.screenshot({ path: `${shots}/p6-comment.png` }));

// Private repo: anonymous request is a 404.
const src = await img.getAttribute("src");
const anon = await fetch(src);
check(
  anon.status === 404,
  `anonymous fetch of private attachment is 404 (${anon.status})`,
);

// Attach button exists in the toolbar.
check(
  await page.getByRole("button", { name: "Attach files" }).first().isVisible(),
  "Attach files button visible",
);

// Wiki editor paste.
await page.goto(`${base}/ada/demo/wiki/new`);
const wiki = page.getByLabel("Page content");
await wiki.waitFor({ timeout: 15000 }).catch(() => null);
if (await wiki.isVisible().catch(() => false)) {
  await wiki.evaluate((el, b64) => {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    const dt = new DataTransfer();
    dt.items.add(new File([bytes], "diagram.png", { type: "image/png" }));
    el.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData: dt,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, png.toString("base64"));
  await page.waitForFunction(
    () =>
      /!\[diagram\.png\]\(/.test(
        document.querySelector('textarea[aria-label="Page content"]')?.value ??
          "",
      ),
    null,
    { timeout: 15000 },
  );
  check(true, "wiki editor uploads pasted images");
} else check(false, "wiki editor reachable");

// Release notes editor paste.
await page.goto(`${base}/ada/demo/releases/new`);
const notes = page.getByLabel("Release notes");
await notes.waitFor({ timeout: 15000 });
await notes.evaluate((el) => {
  const dt = new DataTransfer();
  dt.items.add(new File(["log line\n"], "build.log", { type: "text/plain" }));
  el.dispatchEvent(
    new DragEvent("drop", {
      dataTransfer: dt,
      bubbles: true,
      cancelable: true,
    }),
  );
});
await page.waitForFunction(
  () =>
    /\[build\.log\]\(http[^)]+\/user-attachments\/files\/\d+\/build\.log\)/.test(
      document.querySelector('textarea[aria-label="Release notes"]')?.value ??
        "",
    ),
  null,
  { timeout: 15000 },
);
check(true, "release notes editor uploads dropped files");

await browser.close();
console.log(failures ? `${failures} failure(s)` : "all good");
process.exit(failures ? 1 : 0);
