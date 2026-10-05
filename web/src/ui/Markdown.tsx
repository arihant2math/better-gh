import { useEffect, useReducer, type MouseEvent } from 'react';
import { navigate, prefetch } from '../router';
import { cx } from './Button';
import './markdown.css';
import type { RenderContext } from './markdown/render';

type Renderer = (src: string, ctx?: RenderContext) => string;

let renderer: Renderer | null = null;
let loading: Promise<void> | null = null;
const cache = new Map<string, string>();

/** Start loading the markdown chunk (call on intent, e.g. hovering an issue). */
export function preloadMarkdown(): Promise<void> {
  loading ??= import('./markdown/render').then((m) => {
    renderer = m.renderMarkdown;
  });
  return loading;
}

function render(src: string, ctx: RenderContext): string {
  const key = `${ctx.repo ?? ''}\n${src}`;
  let html = cache.get(key);
  if (html === undefined) {
    html = renderer!(src, ctx);
    cache.set(key, html);
    if (cache.size > 600) cache.delete(cache.keys().next().value!);
  }
  return html;
}

function onClick(e: MouseEvent<HTMLDivElement>) {
  const a = (e.target as HTMLElement).closest('a');
  if (!a || a.target || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
  const href = a.getAttribute('href');
  if (href?.startsWith('/')) {
    e.preventDefault();
    navigate(href);
  }
}

function onPointerOver(e: MouseEvent<HTMLDivElement>) {
  const href = (e.target as HTMLElement).closest('a')?.getAttribute('href');
  if (href?.startsWith('/')) prefetch(href);
}

/**
 * Renders GitHub-flavored markdown (sanitized). Until the renderer chunk is
 * loaded it shows the raw text, so content is never blank.
 */
export function Markdown({ source, repo, className }: { source: string; repo?: string; className?: string }) {
  const [, force] = useReducer((x: number) => x + 1, 0);
  useEffect(() => {
    if (!renderer) void preloadMarkdown().then(force);
  }, []);
  if (!source.trim()) {
    return <div className={cx('markdown-body', 'markdown-empty', className)}>No description provided.</div>;
  }
  if (!renderer) {
    return (
      <div className={cx('markdown-body', className)}>
        <p style={{ whiteSpace: 'pre-wrap' }}>{source}</p>
      </div>
    );
  }
  return (
    <div
      className={cx('markdown-body', className)}
      onClick={onClick}
      onPointerOver={onPointerOver}
      dangerouslySetInnerHTML={{ __html: render(source, { repo }) }}
    />
  );
}
