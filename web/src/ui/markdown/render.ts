/**
 * GitHub-flavored markdown → sanitized HTML. Lazy chunk (marked + DOMPurify,
 * ~25 KB gzip): import through `ui/Markdown`, never statically from the shell.
 *
 * Kept in parity with the server's comrak renderer (bgh_core::markdown) —
 * alerts, footnotes, heading anchors, references, autolinks, emoji, math
 * markers, task lists — by the shared corpus in `testdata/markdown/`
 * (`render.golden.test.ts`). Highlighting, math typesetting, Mermaid and
 * the camo image proxy are applied after rendering by `enhance.ts`.
 */
import DOMPurify from 'dompurify';
import { Marked, type Token, type Tokens, type TokenizerAndRendererExtension } from 'marked';
import { anchorizer, scan, shortUrl, type AutolinkRule } from './scan';
import { attachmentVideo } from './video';

export type { AutolinkRule } from './scan';

export interface RenderContext {
  /** `owner/repo` for resolving `#123`, `GH-123` and SHAs. */
  repo?: string;
  /** Link prefix for references: `''` (default) for app-relative links. */
  base?: string;
  /** This instance's origin, for shortening issue/commit URLs (default `location.origin`). */
  origin?: string;
  /** Repository custom autolinks. */
  autolinks?: readonly AutolinkRule[];
  /** Render task checkboxes enabled (the viewer may edit the source). */
  tasks?: boolean;
}

let emojiTable: Readonly<Record<string, string>> | null = null;

/** Install the gemoji table (`emoji.json`, a separate lazy chunk). */
export function setEmoji(table: Readonly<Record<string, string>>) {
  emojiTable = table;
}

const ALERTS: Record<string, string> = { note: 'Note', tip: 'Tip', important: 'Important', warning: 'Warning', caution: 'Caution' };
const ALERT = /^\[!(note|tip|important|warning|caution)\][ \t]*(?:\n|$)/i;

const escapeHtml = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

/** Per-parse state (renders are synchronous, one at a time). */
interface ParseState {
  slug: (text: string) => string;
  /** Footnote label → definition tokens. */
  defs: Map<string, Token[]>;
  /** Referenced footnote labels in first-reference order. */
  order: string[];
}

function markedInstance(): Marked {
  let st: ParseState = { slug: anchorizer(), defs: new Map(), order: [] };
  const m = new Marked({ gfm: true, breaks: true, async: false });

  const footnoteDef: TokenizerAndRendererExtension = {
    name: 'footnoteDef',
    level: 'block',
    start: (src) => src.match(/^ {0,3}\[\^[^\]\s]+\]:/m)?.index,
    tokenizer(src) {
      const m = /^ {0,3}\[\^([^\]\s]+)\]:[ \t]*([^\n]*(?:\n(?![ \t]*\n| {0,3}\[\^)[^\n]*|\n[ \t]*\n(?=(?: {2,}|\t)\S)[^\n]*)*)\n?/.exec(src);
      if (!m) return undefined;
      const body = m[2]!.replace(/\n(?: {1,4}|\t)/g, '\n');
      const tokens: Token[] = [];
      this.lexer.blockTokens(body, tokens);
      if (!st.defs.has(m[1]!)) st.defs.set(m[1]!, tokens);
      return { type: 'footnoteDef', raw: m[0], label: m[1], tokens: [] };
    },
    renderer: () => '',
  };
  const footnoteRef: TokenizerAndRendererExtension = {
    name: 'footnoteRef',
    level: 'inline',
    start: (src) => src.match(/\[\^/)?.index,
    tokenizer(src) {
      const m = /^\[\^([^\]\s]+)\]/.exec(src);
      return m && st.defs.has(m[1]!) ? { type: 'footnoteRef', raw: m[0], label: m[1] } : undefined;
    },
    renderer(t) {
      const label = t.label as string;
      let n = st.order.indexOf(label) + 1;
      if (!n) n = st.order.push(label);
      const id = escapeHtml(label);
      return `<sup class="footnote-ref"><a href="#user-content-fn-${id}" id="user-content-fnref-${id}">${n}</a></sup>`;
    },
  };
  const math: TokenizerAndRendererExtension = {
    name: 'math',
    level: 'inline',
    start: (src) => src.match(/\$/)?.index,
    tokenizer(src) {
      let m = /^\$\$(?!\$)([\s\S]+?)\$\$/.exec(src);
      if (m) return { type: 'math', raw: m[0], text: m[1], display: true };
      m = /^\$(?![\s$])((?:\\.|[^$\\])+?)\$(?!\d)/.exec(src);
      if (m && !/\s$/.test(m[1]!)) return { type: 'math', raw: m[0], text: m[1], display: false };
      return undefined;
    },
    renderer: (t) => `<span data-math-style="${t.display ? 'display' : 'inline'}">${escapeHtml(t.text as string)}</span>`,
  };
  m.use({
    extensions: [footnoteDef, footnoteRef, math],
    hooks: {
      preprocess(src) {
        st = { slug: anchorizer(), defs: new Map(), order: [] };
        return src;
      },
      postprocess(html) {
        if (!st.order.length) return html;
        let out = '<section class="footnotes">\n<ol>\n';
        // Definitions may reference further footnotes: iterate as order grows.
        for (let i = 0; i < st.order.length; i++) {
          const label = st.order[i]!;
          const id = escapeHtml(label);
          const back = ` <a href="#user-content-fnref-${id}" class="footnote-backref">↩</a>`;
          let body = m.parser(st.defs.get(label) ?? []);
          body = body.endsWith('</p>\n') ? `${body.slice(0, -5)}${back}</p>\n` : body + back;
          out += `<li id="user-content-fn-${id}">\n${body}</li>\n`;
        }
        return `${html}${out}</ol>\n</section>\n`;
      },
    },
    renderer: {
      paragraph({ text }) {
        return attachmentVideo(text) ?? false;
      },
      heading({ tokens, depth }) {
        const slug = st.slug(this.parser.parseInline(tokens, this.parser.textRenderer));
        return `<h${depth} id="user-content-${escapeHtml(slug)}">${this.parser.parseInline(tokens)}<a href="#${escapeHtml(slug)}" class="anchor"></a></h${depth}>\n`;
      },
      blockquote({ text }) {
        const a = ALERT.exec(text);
        if (!a) return false;
        const kind = a[1]!.toLowerCase();
        const inner = m.parser(m.lexer(text.slice(a[0].length)));
        return `<div class="markdown-alert markdown-alert-${kind}">\n<p class="markdown-alert-title">${ALERTS[kind]}</p>\n${inner}</div>\n`;
      },
      code({ text, lang }) {
        const info = (lang ?? '').match(/^\S*/)?.[0] ?? '';
        const body = text ? escapeHtml(text.replace(/\n$/, '')) + '\n' : '';
        return info ? `<pre lang="${escapeHtml(info)}"><code>${body}</code></pre>\n` : `<pre><code>${body}</code></pre>\n`;
      },
      list(token: Tokens.List) {
        const tag = token.ordered ? 'ol' : 'ul';
        const start = token.ordered && token.start !== 1 ? ` start="${token.start}"` : '';
        const cls = token.items.some((i) => i.task) ? ' class="contains-task-list"' : '';
        return `<${tag}${start}${cls}>\n${token.items.map((i) => this.listitem(i)).join('')}</${tag}>\n`;
      },
      listitem(item) {
        return `<li${item.task ? ' class="task-list-item"' : ''}>${this.parser.parse(item.tokens)}</li>\n`;
      },
      checkbox({ checked }) {
        return `<input type="checkbox" class="task-list-item-checkbox"${checked ? ' checked=""' : ''} disabled=""> `;
      },
      link({ href, title, tokens }) {
        const text = this.parser.parseInline(tokens);
        const external = /^https?:\/\//.test(href);
        const t = title ? ` title="${escapeHtml(title)}"` : '';
        return `<a href="${escapeHtml(href)}"${t}${external ? ' target="_blank" rel="noopener noreferrer nofollow"' : ''}>${text}</a>`;
      },
    },
  });
  return m;
}

let instance: Marked | null = null;

let tasksEnabled = false;
DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  const el = node as Element;
  switch (el.tagName) {
    case 'INPUT':
      if (!tasksEnabled || !el.classList.contains('task-list-item-checkbox')) el.setAttribute('disabled', '');
      else el.removeAttribute('disabled');
      break;
    case 'IMG':
      el.setAttribute('loading', 'lazy');
      el.setAttribute('decoding', 'async');
      break;
  }
  // Author ids can't clobber the app's (comrak/ammonia prefix them too).
  const id = el.getAttribute?.('id');
  if (id && !id.startsWith('user-content-')) el.setAttribute('id', `user-content-${id}`);
});

const SKIP = new Set(['A', 'CODE', 'PRE', 'SCRIPT', 'STYLE', 'G-EMOJI']);

/** Link references, mentions, autolinks and emoji in text nodes; shorten instance URLs. */
function linkText(root: DocumentFragment, ctx: RenderContext) {
  const doc = root.ownerDocument;
  const [owner, name] = ctx.repo?.split('/') ?? [];
  const sc = {
    base: ctx.base ?? '',
    repo: owner && name ? ([owner, name] as const) : undefined,
    autolinks: ctx.autolinks,
    emoji: emojiTable,
  };
  const texts: Text[] = [];
  const walker = doc.createTreeWalker(root, 4 /* NodeFilter.SHOW_TEXT */);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    let skip = false;
    for (let p = n.parentElement; p; p = p.parentElement) {
      if (SKIP.has(p.tagName) || p.hasAttribute('data-math-style')) {
        skip = true;
        break;
      }
    }
    if (!skip) texts.push(n as Text);
  }
  for (const t of texts) {
    const segs = scan(t.data, sc);
    if (segs.every((s) => s.kind === 'text')) continue;
    const frag = doc.createDocumentFragment();
    for (const s of segs) {
      if (s.kind === 'text') frag.append(s.text);
      else if (s.kind === 'emoji') {
        const g = doc.createElement('g-emoji');
        g.className = 'g-emoji';
        g.setAttribute('alias', s.name);
        g.textContent = s.emoji;
        frag.append(g);
      } else {
        const a = doc.createElement('a');
        a.className = s.cls;
        a.setAttribute('href', s.url);
        if (/^https?:\/\//.test(s.url) && !sc.base) {
          a.setAttribute('target', '_blank');
          a.setAttribute('rel', 'noopener noreferrer nofollow');
        }
        a.textContent = s.text;
        frag.append(a);
      }
    }
    t.replaceWith(frag);
  }
  const origin = ctx.origin ?? (ctx.base || (typeof location === 'undefined' ? '' : location.origin));
  if (!origin) return;
  for (const a of root.querySelectorAll('a[href]')) {
    const href = a.getAttribute('href')!;
    if (a.textContent !== href || a.children.length) continue;
    const short = shortUrl(origin, sc.repo, href);
    if (!short) continue;
    a.className = short.cls;
    a.textContent = short.text;
    if (!ctx.base) {
      a.setAttribute('href', href.slice(origin.replace(/\/+$/, '').length));
      a.removeAttribute('target');
      a.removeAttribute('rel');
    }
  }
}

export function renderMarkdown(src: string, ctx: RenderContext = {}): string {
  instance ??= markedInstance();
  const html = instance.parse(src) as string;
  tasksEnabled = !!ctx.tasks;
  const clean = DOMPurify.sanitize(html, {
    ADD_TAGS: ['g-emoji'],
    ADD_ATTR: ['target', 'controls', 'preload', 'loading', 'decoding', 'alias', 'lang'],
    FORBID_TAGS: ['style', 'form'],
  });
  tasksEnabled = false;
  const box = document.createElement('template');
  box.innerHTML = clean;
  linkText(box.content, ctx);
  return box.innerHTML;
}
