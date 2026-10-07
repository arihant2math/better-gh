// Layout checks for scripts/viewport-matrix.mjs. `collectLayoutIssues` runs
// inside the page (serialized by `page.evaluate`), so it must stay
// self-contained: no closures over module scope, no imports.

/**
 * @param {{ touch: boolean, minTap: number }} opts
 * @returns {{ check: string, selector: string, detail: string }[]}
 */
export function collectLayoutIssues(opts) {
  const vw = document.documentElement.clientWidth;
  const vh = window.innerHeight;
  const out = [];
  const push = (check, el, detail) => out.push({ check, selector: el ? selectorOf(el) : 'html', detail });

  function selectorOf(el) {
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && parts.length < 4; e = e.parentElement) {
      if (e === document.body || e === document.documentElement) break;
      let s = e.tagName.toLowerCase();
      if (e.id) {
        parts.unshift(`${s}#${e.id}`);
        break;
      }
      const role = e.getAttribute('role');
      if (role) s += `[role=${role}]`;
      // CSS-module names: `_name_hash_line` (build) or `name__hash` (dev).
      const cls = [...e.classList].slice(0, 2).map((c) => c.replace(/^_([a-zA-Z][\w-]*?)_[0-9a-z]{5}_\d+$/, '$1').replace(/__[\w-]{5,}$/, ''));
      if (cls.length) s += `.${cls.join('.')}`;
      const label = e.getAttribute('aria-label');
      if (label && parts.length === 0) s += `[aria-label="${label.slice(0, 40)}"]`;
      parts.unshift(s);
    }
    return parts.join(' > ') || el.tagName.toLowerCase();
  }

  const styleCache = new Map();
  const style = (el) => {
    let s = styleCache.get(el);
    if (!s) styleCache.set(el, (s = getComputedStyle(el)));
    return s;
  };
  const visible = (el) => {
    const s = style(el);
    if (s.display === 'none' || s.visibility === 'hidden' || s.visibility === 'collapse' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width > 1 && r.height > 1;
  };
  // Visually-hidden helpers (sr-only, clip-path inset) are intentional.
  const srOnly = (el) => {
    for (let e = el; e && e !== document.body; e = e.parentElement) {
      const s = style(e);
      if (s.clip && s.clip !== 'auto') return true;
      if (s.clipPath && s.clipPath.startsWith('inset(50%')) return true;
      const r = e.getBoundingClientRect();
      if (r.width <= 1 && r.height <= 1 && s.overflow === 'hidden') return true;
    }
    return false;
  };
  const clipsX = (s) => s.overflowX !== 'visible';
  const parked = (el) => {
    const r = el.getBoundingClientRect();
    if (!(r.right <= 0 || r.left >= vw)) return false;
    for (let e = el; e && e !== document.body; e = e.parentElement) {
      const p = style(e).position;
      if (p === 'fixed' || p === 'absolute') return true;
    }
    return false;
  };
  // Nearest ancestor that clips or scrolls horizontally (or a fixed/sticky
  // layer, which is positioned against the viewport, not the page).
  const clippingAncestor = (el) => {
    for (let e = el.parentElement; e && e !== document.body && e !== document.documentElement; e = e.parentElement) {
      const s = style(e);
      if (clipsX(s)) return e;
    }
    return null;
  };

  // 1. horizontal page overflow
  const de = document.documentElement;
  const sw = Math.max(de.scrollWidth, document.body ? document.body.scrollWidth : 0);
  if (sw > de.clientWidth + 1) push('page-overflow', null, `scrollWidth ${sw} > clientWidth ${de.clientWidth}`);

  const all = [...document.body.querySelectorAll('*')].filter(
    (el) => !['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT', 'BR', 'WBR'].includes(el.tagName) && !el.closest('svg *'),
  );
  const vis = all.filter(visible);

  // 2. elements sticking out of the viewport (or out of a clipping ancestor
  // that doesn't scroll, i.e. content that can never be reached)
  const flagged = new Set();
  for (const el of vis) {
    if (srOnly(el)) continue;
    const r = el.getBoundingClientRect();
    const anc = clippingAncestor(el);
    let parentFlagged = false;
    for (let e = el.parentElement; e; e = e.parentElement) if (flagged.has(e)) parentFlagged = true;
    if (parentFlagged) continue;
    if (!anc) {
      // Wholly off-screen positioned layers are parked on purpose (skip
      // links, off-canvas drawers); a partly visible one is a bug.
      if (parked(el)) continue;
      if (r.right > vw + 1 || r.left < -1) {
        flagged.add(el);
        push('offscreen', el, `x ${Math.round(r.left)}…${Math.round(r.right)} outside viewport 0…${vw}`);
      }
      continue;
    }
    const as = style(anc);
    if (as.overflowX === 'auto' || as.overflowX === 'scroll') continue; // reachable by scrolling
    // Clipped by overflow:hidden/clip — only a problem for interactive
    // elements, which become unreachable (text clipping is check 3).
    if (!el.matches('a[href],button,input,select,textarea,[role=button],[role=tab],[role=menuitem],[tabindex]:not([tabindex="-1"])')) continue;
    const ar = anc.getBoundingClientRect();
    if (r.width > 0 && (r.left >= ar.right - 1 || r.right <= ar.left + 1) && !srOnly(anc)) {
      flagged.add(el);
      push('unreachable', el, `clipped out of ${selectorOf(anc)} (x ${Math.round(r.left)}…${Math.round(r.right)} vs ${Math.round(ar.left)}…${Math.round(ar.right)})`);
    }
  }

  // 3. text clipped by an overflow:hidden/clip box (its own or an
  // ancestor's) that doesn't end in an ellipsis. Measured per text node so
  // wide blocks with short text don't count.
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  const clippedSeen = new Set();
  const squeezedSeen = new Set();
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const parent = n.parentElement;
    if (!parent || !n.textContent.trim() || clippedSeen.has(parent)) continue;
    if (['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEXTAREA', 'OPTION'].includes(parent.tagName) || !visible(parent) || srOnly(parent)) continue;
    range.selectNodeContents(n);
    const tr = range.getBoundingClientRect();
    if (tr.width <= 1) continue;
    // 3a. text squeezed to a couple of characters per line by its siblings
    // (e.g. a title next to wide header actions).
    const chars = n.textContent.replace(/\s+/g, '').length;
    const lines = new Set([...range.getClientRects()].filter((r) => r.width > 0).map((r) => Math.round(r.top))).size;
    if (chars >= 6 && lines >= 3 && chars / lines < 3 && !squeezedSeen.has(parent)) {
      squeezedSeen.add(parent);
      push('squeezed-text', parent, `${chars} characters on ${lines} lines (${Math.round(tr.width)}px wide): "${n.textContent.trim().slice(0, 40)}"`);
    }
    let anc = null;
    let ellipsis = false;
    for (let e = parent; e && e !== document.body && e !== document.documentElement; e = e.parentElement) {
      const s = style(e);
      if (s.textOverflow === 'ellipsis' || (s.webkitLineClamp && s.webkitLineClamp !== 'none')) ellipsis = true;
      if (clipsX(s)) {
        anc = e;
        break;
      }
    }
    if (!anc || ellipsis) continue;
    const as = style(anc);
    if (as.overflowX === 'auto' || as.overflowX === 'scroll') continue;
    const ar = anc.getBoundingClientRect();
    const cut = Math.max(tr.right - ar.right, ar.left - tr.left);
    if (cut > 1) {
      clippedSeen.add(parent);
      push('clipped-text', parent, `${Math.round(cut)}px cut off by ${anc === parent ? 'its own overflow' : selectorOf(anc)}: "${n.textContent.trim().slice(0, 40)}"`);
    }
  }

  // 4. overlapping interactive elements: the centre of a control is covered
  // by another control (not its ancestor/descendant).
  const interactive = vis.filter(
    (el) =>
      el.matches('a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=tab],[role=menuitem],[role=checkbox]') &&
      !srOnly(el),
  );
  const isInteractive = (el) => interactive.includes(el);
  for (const el of interactive) {
    const r = el.getBoundingClientRect();
    const cx = r.left + r.width / 2;
    const cy = r.top + r.height / 2;
    if (cx < 0 || cy < 0 || cx >= vw || cy >= vh) continue;
    let top = document.elementFromPoint(cx, cy);
    if (!top || el.contains(top) || top.contains(el)) continue;
    // Walk up from the hit to the control it belongs to.
    while (top && !isInteractive(top)) top = top.parentElement;
    if (!top || top.contains(el) || el.contains(top)) continue;
    // Labels wrapping a control, and native inputs styled behind a custom
    // face, legitimately sit underneath.
    if (el.closest('label') && el.closest('label').contains(top)) continue;
    push('overlap', el, `covered by ${selectorOf(top)}`);
  }

  // 5. tap targets on touch viewports
  if (opts.touch) {
    for (const el of interactive) {
      const r = el.getBoundingClientRect();
      if (r.width >= opts.minTap && r.height >= opts.minTap) continue;
      if (r.bottom < 0 || r.top > vh * 3 || parked(el)) continue;
      const s = style(el);
      // Inline links in running text are exempt (WCAG 2.5.8 inline exception).
      if (el.tagName === 'A' && s.display === 'inline') {
        const p = el.parentElement;
        const text = p ? p.textContent.trim().length : 0;
        if (text > el.textContent.trim().length + 2) continue;
      }
      // A small checkbox/radio inside a big enough label is fine.
      const label = el.closest('label');
      if (label) {
        const lr = label.getBoundingClientRect();
        if (lr.width >= opts.minTap && lr.height >= opts.minTap) continue;
      }
      // An undersized control whose (single-control) parent is a big enough
      // hit area — e.g. an icon button inside a padded wrapper with a click
      // handler — can't be detected; report it and let --allow baseline it.
      push('tap-target', el, `${Math.round(r.width)}×${Math.round(r.height)} < ${opts.minTap}px`);
    }
  }

  return out;
}
