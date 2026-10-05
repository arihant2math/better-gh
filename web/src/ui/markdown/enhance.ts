/**
 * Post-render enhancements of rendered Markdown, applied to the live DOM
 * (part of the markdown chunk; the heavy parts are their own lazy chunks
 * and load only when a body needs them):
 *
 * - fenced code highlighting through `POST /_bgh/render/code` (the
 *   server's syntect highlighter, same classes as the code browser);
 * - math (`$…$`, `$$…$$`, ```` ```math ````) typeset to MathML by temml;
 * - ```` ```mermaid ```` diagrams (mermaid, strict security level);
 * - external images through the camo proxy (`POST /_bgh/camo/sign`).
 */
import { api } from '../../api/client';

const SKIP_HIGHLIGHT = new Set(['math', 'mermaid', 'text', 'plaintext', 'txt']);
const highlighted = new Map<string, string | null>();
const signed = new Map<string, string>();

const key = (lang: string, code: string) => `${lang}\0${code}`;

async function highlight(root: HTMLElement) {
  const blocks = [...root.querySelectorAll<HTMLElement>('pre[lang] > code')].filter((c) => {
    const lang = c.parentElement!.getAttribute('lang')!.toLowerCase();
    return !SKIP_HIGHLIGHT.has(lang) && !c.dataset.hl;
  });
  if (!blocks.length) return;
  const items = blocks.map((c) => ({ el: c, lang: c.parentElement!.getAttribute('lang')!, code: c.textContent ?? '' }));
  const missing = items.filter((b) => !highlighted.has(key(b.lang, b.code)));
  const unique = [...new Map(missing.map((b) => [key(b.lang, b.code), b])).values()].slice(0, 50);
  if (unique.length) {
    try {
      const res = await api.post<{ blocks: ({ lines: string[] } | null)[] }>('/_bgh/render/code', {
        blocks: unique.map((b) => ({ lang: b.lang, code: b.code.replace(/\n$/, '') })),
      });
      unique.forEach((b, i) => highlighted.set(key(b.lang, b.code), res.blocks[i]?.lines.join('\n') ?? null));
    } catch {
      unique.forEach((b) => highlighted.set(key(b.lang, b.code), null));
    }
  }
  for (const b of items) {
    const html = highlighted.get(key(b.lang, b.code));
    // Skip blocks replaced by a newer render meanwhile.
    if (!html || !b.el.isConnected || b.el.textContent !== b.code) continue;
    b.el.innerHTML = `${html}\n`;
    b.el.dataset.hl = '1';
  }
}

async function math(root: HTMLElement) {
  const spans = [...root.querySelectorAll<HTMLElement>('[data-math-style]:not([data-typeset])')];
  const fences = [...root.querySelectorAll<HTMLElement>('pre[lang="math"]')];
  if (!spans.length && !fences.length) return;
  const temml = await import('temml');
  const typeset = (el: HTMLElement, tex: string, displayMode: boolean) => {
    try {
      temml.render(tex.trim(), el, { displayMode, throwOnError: false });
    } catch {
      el.textContent = tex;
      el.classList.add('render-error');
    }
  };
  for (const s of spans) {
    s.dataset.typeset = '1';
    typeset(s, s.textContent ?? '', s.dataset.mathStyle === 'display');
  }
  for (const pre of fences) {
    const div = document.createElement('div');
    div.className = 'math-display';
    typeset(div, pre.textContent ?? '', true);
    pre.replaceWith(div);
  }
}

let mermaidSeq = 0;

async function mermaid(root: HTMLElement) {
  const pres = [...root.querySelectorAll<HTMLElement>('pre[lang="mermaid"]')];
  if (!pres.length) return;
  const { default: m } = await import('mermaid');
  const dark = matchMedia('(prefers-color-scheme: dark)').matches || document.documentElement.dataset.theme === 'dark';
  m.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default' });
  for (const pre of pres) {
    const code = pre.textContent ?? '';
    const div = document.createElement('div');
    div.className = 'mermaid-diagram';
    try {
      const { svg } = await m.render(`bgh-mermaid-${++mermaidSeq}`, code);
      if (!pre.isConnected) continue;
      div.innerHTML = svg;
      pre.replaceWith(div);
    } catch (e) {
      const err = document.createElement('p');
      err.className = 'render-error';
      err.textContent = `Unable to render the Mermaid diagram: ${e instanceof Error ? e.message : String(e)}`;
      pre.after(err);
    }
  }
}

async function camo(root: HTMLElement) {
  const imgs = [...root.querySelectorAll<HTMLImageElement>('img[data-canonical-src]:not([src])')];
  if (!imgs.length) return;
  const urls = [...new Set(imgs.map((i) => i.dataset.canonicalSrc!))];
  const todo = urls.filter((u) => !signed.has(u)).slice(0, 100);
  if (todo.length) {
    try {
      const res = await api.post<{ urls: Record<string, string> }>('/_bgh/camo/sign', { urls: todo });
      for (const u of todo) signed.set(u, res.urls[u] ?? u);
    } catch {
      // Proxy unavailable: show the image directly rather than not at all.
      for (const u of todo) signed.set(u, u);
    }
  }
  for (const img of imgs) {
    const url = signed.get(img.dataset.canonicalSrc!);
    if (url) img.src = url;
  }
}

/** Apply all enhancements to a rendered `.markdown-body`. */
export function enhance(root: HTMLElement): void {
  void camo(root);
  void highlight(root);
  void math(root);
  void mermaid(root);
}
