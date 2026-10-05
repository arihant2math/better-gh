/**
 * GitHub-flavored markdown → sanitized HTML. Lazy chunk (marked + DOMPurify,
 * ~20 KB gzip): import through `ui/Markdown`, never statically from the shell.
 */
import DOMPurify from 'dompurify';
import { Marked, type TokenizerAndRendererExtension } from 'marked';

const EMOJI: Record<string, string> = {
  '+1': '👍',
  thumbsup: '👍',
  '-1': '👎',
  rocket: '🚀',
  tada: '🎉',
  heart: '❤️',
  eyes: '👀',
  smile: '😄',
  laughing: '😆',
  confused: '😕',
  warning: '⚠️',
  bug: '🐛',
  fire: '🔥',
  sparkles: '✨',
  white_check_mark: '✅',
  x: '❌',
  memo: '📝',
  zap: '⚡',
};

export interface RenderContext {
  /** `owner/repo` for resolving `#123` references. */
  repo?: string;
}

function extensions(ctx: RenderContext): TokenizerAndRendererExtension[] {
  return [
    {
      name: 'mention',
      level: 'inline',
      start: (src) => src.match(/(^|[^\w`])@[a-z\d]/i)?.index,
      tokenizer(src) {
        const m = /^@([a-z\d](?:[a-z\d]|-(?=[a-z\d])){0,38})(?![\w-])/i.exec(src);
        return m ? { type: 'mention', raw: m[0], login: m[1] } : undefined;
      },
      renderer: (t) => `<a class="user-mention" href="/${encodeURIComponent(t.login as string)}">@${t.login as string}</a>`,
    },
    {
      name: 'issueRef',
      level: 'inline',
      start: (src) => src.match(/#\d/)?.index,
      tokenizer(src) {
        const m = /^#(\d{1,7})\b/.exec(src);
        return m && ctx.repo ? { type: 'issueRef', raw: m[0], n: m[1] } : undefined;
      },
      renderer: (t) => `<a class="issue-link" href="/${ctx.repo}/issues/${t.n as string}">#${t.n as string}</a>`,
    },
    {
      name: 'emoji',
      level: 'inline',
      start: (src) => src.match(/:[\w+-]+:/)?.index,
      tokenizer(src) {
        const m = /^:([\w+-]+):/.exec(src);
        return m && EMOJI[m[1]!] ? { type: 'emoji', raw: m[0], name: m[1] } : undefined;
      },
      renderer: (t) => `<g-emoji title=":${t.name as string}:">${EMOJI[t.name as string]}</g-emoji>`,
    },
  ];
}

const instances = new Map<string, Marked>();

function markedFor(ctx: RenderContext): Marked {
  const key = ctx.repo ?? '';
  let m = instances.get(key);
  if (!m) {
    m = new Marked({ gfm: true, breaks: true, async: false });
    m.use({ extensions: extensions(ctx) });
    m.use({
      renderer: {
        link({ href, title, tokens }) {
          const text = this.parser.parseInline(tokens);
          const external = /^https?:\/\//.test(href);
          const t = title ? ` title="${title}"` : '';
          return `<a href="${href}"${t}${external ? ' target="_blank" rel="noopener noreferrer nofollow"' : ''}>${text}</a>`;
        },
      },
    });
    instances.set(key, m);
  }
  return m;
}

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'INPUT') node.setAttribute('disabled', '');
});

export function renderMarkdown(src: string, ctx: RenderContext = {}): string {
  const html = markedFor(ctx).parse(src) as string;
  return DOMPurify.sanitize(html, {
    ADD_TAGS: ['g-emoji'],
    ADD_ATTR: ['target'],
    FORBID_TAGS: ['style', 'form'],
  });
}
