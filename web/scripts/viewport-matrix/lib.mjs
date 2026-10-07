// Node-side helpers for scripts/viewport-matrix.mjs (argument parsing,
// viewport table, --allow baselines, summary). Kept free of Playwright so
// the unit tests run anywhere.
import { readFileSync } from 'node:fs';

/** The device matrix from docs/AGENT_WORKFLOW.md → "Device testing". */
export const VIEWPORTS = [
  { name: 'phone-s', width: 360, height: 740, touch: true, mobile: true, dpr: 3 },
  { name: 'phone', width: 390, height: 844, touch: true, mobile: true, dpr: 3 },
  { name: 'tablet-p', width: 768, height: 1024, touch: true, mobile: true, dpr: 2 },
  { name: 'tablet-l', width: 1024, height: 768, touch: true, mobile: true, dpr: 2 },
  { name: 'laptop', width: 1280, height: 800, touch: false, mobile: false, dpr: 2 },
  { name: 'desktop', width: 1440, height: 900, touch: false, mobile: false, dpr: 1 },
  { name: 'fhd', width: 1920, height: 1080, touch: false, mobile: false, dpr: 1 },
  { name: 'ultrawide', width: 2560, height: 1080, touch: false, mobile: false, dpr: 1 },
  { name: 'portrait', width: 1080, height: 1920, touch: false, mobile: false, dpr: 1 },
];

/** Widths visited by the live-resize pass (desktop height). */
export const RESIZE_STEPS = [1440, 1280, 1100, 1024, 900, 768, 640, 540, 480, 420, 390, 360];

/** Main routes of the seeded real backend (web/scripts/seed-real.mjs). */
export const DEFAULT_ROUTES = ['/', '/notifications', '/acme', '/ada', '/acme/api', '/acme/api/issues', '/acme/api/issues/1', '/acme/api/pulls', '/settings'];

export const CHECKS = ['page-overflow', 'offscreen', 'unreachable', 'clipped-text', 'overlap', 'tap-target', 'console', 'request', 'load'];

export const USAGE = `usage: node scripts/viewport-matrix.mjs --base <url> [options]

  --base <url>          server to test (default http://localhost:3000)
  --routes a,b,c        routes (default: ${DEFAULT_ROUTES.join(',')})
  --out <dir>           screenshots + report.json (default test-results/viewports)
  --themes light,dark   colour schemes to emulate (default both)
  --viewports a,b       only these viewports (names or widths, e.g. phone,1440)
  --login user:pass     sign in through /login first
  --mock                use the in-browser mock backend (adds ?mock to URLs)
  --no-resize           skip the live-resize pass (on by default; --resize to force)
  --allow <file>        baseline of known issues; matching ones don't fail the run
  --write-allow <file>  write every issue found as a baseline and exit 0
  --local-storage k=v   seed a localStorage entry before load (repeatable)
  --wait-for <sel>      CSS selector every route must render before checking
  --min-tap <px>        minimum touch target size (default 32)
  --jobs <n>            parallel browser contexts (default 4)
  --full-page           full-page screenshots instead of the viewport
  --checks a,b          only run these checks (${CHECKS.join(', ')})
`;

export function parseArgs(argv) {
  const o = {
    base: 'http://localhost:3000',
    routes: DEFAULT_ROUTES,
    out: 'test-results/viewports',
    themes: ['light', 'dark'],
    viewports: VIEWPORTS,
    login: null,
    localStorage: {},
    mock: false,
    resize: true,
    allow: null,
    writeAllow: null,
    waitFor: null,
    minTap: 32,
    jobs: 4,
    fullPage: false,
    checks: CHECKS,
    help: false,
  };
  const list = (v) => v.split(',').map((s) => s.trim()).filter(Boolean);
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const [flag, inline] = a.startsWith('--') && a.includes('=') ? [a.slice(0, a.indexOf('=')), a.slice(a.indexOf('=') + 1)] : [a, undefined];
    const val = () => {
      const v = inline ?? argv[++i];
      if (v === undefined) throw new Error(`${flag} needs a value`);
      return v;
    };
    switch (flag) {
      case '--base': o.base = val().replace(/\/$/, ''); break;
      case '--routes': o.routes = list(val()).map((r) => (r.startsWith('/') ? r : `/${r}`)); break;
      case '--out': o.out = val(); break;
      case '--themes': {
        o.themes = list(val());
        const bad = o.themes.filter((t) => t !== 'light' && t !== 'dark');
        if (bad.length) throw new Error(`unknown theme ${bad.join(',')}`);
        break;
      }
      case '--viewports': {
        const want = list(val());
        o.viewports = VIEWPORTS.filter((v) => want.includes(v.name) || want.includes(String(v.width)) || want.includes(`${v.width}x${v.height}`));
        if (!o.viewports.length) throw new Error(`no viewport matches ${want.join(',')}`);
        break;
      }
      case '--login': {
        const v = val();
        const at = v.indexOf(':');
        if (at < 1) throw new Error('--login expects user:pass');
        o.login = { user: v.slice(0, at), pass: v.slice(at + 1) };
        break;
      }
      case '--local-storage': {
        const v = val();
        const eq = v.indexOf('=');
        if (eq < 1) throw new Error('--local-storage expects key=value');
        o.localStorage[v.slice(0, eq)] = v.slice(eq + 1);
        break;
      }
      case '--mock': o.mock = true; break;
      case '--resize': o.resize = true; break;
      case '--no-resize': o.resize = false; break;
      case '--allow': o.allow = val(); break;
      case '--write-allow': o.writeAllow = val(); break;
      case '--wait-for': o.waitFor = val(); break;
      case '--min-tap': o.minTap = Number(val()); break;
      case '--jobs': o.jobs = Math.max(1, Number(val()) || 1); break;
      case '--full-page': o.fullPage = true; break;
      case '--checks': {
        o.checks = list(val());
        const bad = o.checks.filter((c) => !CHECKS.includes(c));
        if (bad.length) throw new Error(`unknown check ${bad.join(',')}`);
        break;
      }
      case '-h':
      case '--help': o.help = true; break;
      default: throw new Error(`unknown argument ${a}`);
    }
  }
  if (/[?&]mock\b/.test(o.base)) {
    o.mock = true;
    o.base = o.base.replace(/[?#].*$/, '').replace(/\/$/, '');
  }
  return o;
}

/** URL for a route; mock mode keeps the mock backend fast and deterministic. */
export function routeUrl(base, route, mock) {
  if (!mock) return base + route;
  const sep = route.includes('?') ? '&' : '?';
  return `${base}${route}${sep}mock&live=0&latency=0`;
}

/** File-name-safe slug: `/acme/api/issues` → `acme-api-issues`, `/` → `root`. */
export function slug(route) {
  return route.replace(/^\/+|\/+$/g, '').replace(/[^\w.-]+/g, '-') || 'root';
}

/**
 * Baseline file: one rule per line, `check route viewport theme selector`,
 * whitespace-separated, `*` matches anything (also inside a field), and
 * missing trailing fields match anything. `#` starts a comment.
 *
 *   tap-target * phone-s,phone * *            # known: small icon buttons
 *   clipped-text /acme/api/issues * dark nav.Tabs*
 */
export function parseAllow(text) {
  const rules = [];
  for (const raw of text.split('\n')) {
    const line = raw.replace(/(^|\s)#.*$/, '').trim();
    if (!line) continue;
    const f = line.split(/\s+/);
    // The selector may itself contain spaces (`a > b`): everything after the
    // fourth field is the selector.
    const fields = [f[0], f[1], f[2], f[3], f.length > 4 ? f.slice(4).join(' ') : undefined];
    rules.push(fields.map((x) => (x === undefined || x === '*' ? null : x)));
  }
  return rules;
}

export function loadAllow(path) {
  return path ? parseAllow(readFileSync(path, 'utf8')) : [];
}

const glob = (pat, s) => {
  const re = new RegExp(`^${pat.split('*').map((p) => p.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('.*')}$`);
  return re.test(s);
};
const fieldMatch = (pat, s) => pat === null || pat.split(',').some((p) => glob(p, s));

/** @param {{check:string,route:string,viewport:string,theme:string,selector:string}} issue */
export function isAllowed(rules, issue) {
  return rules.some((r) =>
    [issue.check, issue.route, issue.viewport, issue.theme, issue.selector].every((v, i) => fieldMatch(r[i], v ?? '')),
  );
}

export function allowLine(issue) {
  const sel = issue.selector.replace(/\s+>\s+/g, ' > ');
  return `${issue.check} ${issue.route} ${issue.viewport} ${issue.theme} ${sel}`;
}

/**
 * Compact stdout summary: one row per route × theme, one column per
 * viewport (+ resize), each cell `ok` or the failing-issue count.
 */
export function summaryTable(results, viewportNames) {
  const cols = [...viewportNames];
  if (results.some((r) => r.viewport === 'resize')) cols.push('resize');
  const rows = new Map();
  for (const r of results) {
    const key = `${r.route} ${r.theme}`;
    if (!rows.has(key)) rows.set(key, {});
    const fails = r.issues.filter((i) => !i.allowed).length;
    const allowed = r.issues.length - fails;
    rows.get(key)[r.viewport] = fails ? String(fails) : allowed ? `ok(${allowed})` : 'ok';
  }
  const head = ['route theme', ...cols];
  const body = [...rows].map(([k, v]) => [k, ...cols.map((c) => v[c] ?? '-')]);
  const w = head.map((h, i) => Math.max(h.length, ...body.map((b) => b[i].length)));
  const fmt = (row) => row.map((c, i) => c.padEnd(w[i])).join('  ');
  return [fmt(head), fmt(w.map((n) => '-'.repeat(n))), ...body.map(fmt)].join('\n');
}

/** Merge repeats of the same check on the same selector (list rows etc.). */
export function collapse(issues) {
  const by = new Map();
  for (const i of issues) {
    const key = `${i.check}\u0000${i.selector}`;
    const prev = by.get(key);
    if (prev) prev.count++;
    else by.set(key, { ...i, count: 1 });
  }
  return [...by.values()].map(({ count, ...i }) => (count > 1 ? { ...i, detail: `${i.detail} (×${count})` } : i));
}
