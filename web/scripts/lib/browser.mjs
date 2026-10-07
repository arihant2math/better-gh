// The one Playwright entry point for scripts/*.mjs. `playwright-core` is a
// pinned devDependency (no bundled browser download); point it at the
// preinstalled browsers with PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers and
// never run `playwright install`. Bump the pin together with that revision.
export { chromium, request } from 'playwright-core';
