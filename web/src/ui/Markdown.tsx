import { useEffect, useReducer, useRef, type MouseEvent } from 'react';
import { api } from '../api/client';
import { navigate, prefetch } from '../router';
import { cx } from './Button';
import './markdown.css';
import type * as RenderChunk from './markdown/render';
import type { AutolinkRule, RenderContext } from './markdown/render';

type Chunk = typeof RenderChunk;

let chunk: Chunk | null = null;
let loading: Promise<void> | null = null;
const cache = new Map<string, string>();

/** Start loading the markdown chunks (call on intent, e.g. hovering an issue). */
export function preloadMarkdown(): Promise<void> {
  loading ??= Promise.all([import('./markdown/render'), import('./markdown/emoji.json?raw')]).then(([render, emoji]) => {
    render.setEmoji(JSON.parse(emoji.default) as Record<string, string>);
    chunk = render;
  });
  return loading;
}

/** Repository autolinks (`GET /_bgh/repos/{o}/{r}/autolinks`), fetched once per repo per session. */
const autolinks = new Map<string, readonly AutolinkRule[] | Promise<void>>();

function repoAutolinks(repo: string | undefined, onLoad: () => void): readonly AutolinkRule[] | undefined {
  if (!repo || !repo.includes('/')) return undefined;
  const hit = autolinks.get(repo);
  if (Array.isArray(hit)) return hit;
  if (!hit) {
    const [owner, name] = repo.split('/') as [string, string];
    autolinks.set(
      repo,
      api
        .get<AutolinkRule[]>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/autolinks`)
        .catch(() => [] as AutolinkRule[])
        .then((rules) => {
          autolinks.set(repo, rules);
          if (rules.length) onLoad();
        }),
    );
  } else {
    void (hit as Promise<void>).then(() => {
      if ((autolinks.get(repo) as readonly AutolinkRule[]).length) onLoad();
    });
  }
  return undefined;
}

function render(src: string, ctx: RenderContext): string {
  const key = `${ctx.repo ?? ''}\n${ctx.autolinks?.length ?? 0}\n${ctx.tasks ? 1 : 0}\n${src}`;
  let html = cache.get(key);
  if (html === undefined) {
    html = chunk!.renderMarkdown(src, ctx);
    cache.set(key, html);
    if (cache.size > 600) cache.delete(cache.keys().next().value!);
  }
  return html;
}

function onPointerOver(e: MouseEvent<HTMLDivElement>) {
  const href = (e.target as HTMLElement).closest('a')?.getAttribute('href');
  if (href?.startsWith('/')) prefetch(href);
}

/**
 * Renders GitHub-flavored markdown (sanitized). Until the renderer chunk is
 * loaded it shows the raw text, so content is never blank.
 *
 * With `onSourceChange` (the viewer can edit the body: author or write
 * access), task-list checkboxes are enabled and ticking one calls it with
 * the source rewritten; callers save it through their edit mutation.
 */
export function Markdown({
  source,
  repo,
  className,
  onSourceChange,
}: {
  source: string;
  repo?: string;
  className?: string;
  onSourceChange?: (next: string) => void;
}) {
  const [, force] = useReducer((x: number) => x + 1, 0);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!chunk) void preloadMarkdown().then(force);
  }, []);
  const rules = repoAutolinks(repo, force);
  const html = chunk && source.trim() ? render(source, { repo, autolinks: rules, tasks: !!onSourceChange }) : null;
  useEffect(() => {
    if (html !== null && ref.current) chunk!.enhance(ref.current);
  }, [html]);

  if (!source.trim()) {
    return <div className={cx('markdown-body', 'markdown-empty', className)}>No description provided.</div>;
  }
  if (html === null) {
    return (
      <div className={cx('markdown-body', className)}>
        <p style={{ whiteSpace: 'pre-wrap' }}>{source}</p>
      </div>
    );
  }
  const onClick = (e: MouseEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    if (target instanceof HTMLInputElement && target.classList.contains('task-list-item-checkbox')) {
      if (!onSourceChange || target.disabled) return;
      const boxes = [...e.currentTarget.querySelectorAll('input.task-list-item-checkbox')];
      const next =
        boxes.length === chunk!.countTasks(source) ? chunk!.setTask(source, boxes.indexOf(target), target.checked) : null;
      if (next === null) {
        // Rendered and source task lists disagree (e.g. tasks in raw HTML): don't guess.
        e.preventDefault();
        return;
      }
      onSourceChange(next);
      return;
    }
    const a = target.closest('a');
    if (!a || a.target || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    const href = a.getAttribute('href');
    if (href?.startsWith('#') && href.length > 1) {
      // Heading anchors and footnotes: ids carry the `user-content-` prefix.
      const id = decodeURIComponent(href.slice(1));
      const el = document.getElementById(id.startsWith('user-content-') ? id : `user-content-${id}`);
      if (el) {
        e.preventDefault();
        el.scrollIntoView({ block: 'start' });
        history.replaceState(history.state, '', href);
      }
      return;
    }
    if (href?.startsWith('/')) {
      e.preventDefault();
      navigate(href);
    }
  };
  return (
    <div
      ref={ref}
      className={cx('markdown-body', className)}
      onClick={onClick}
      onPointerOver={onPointerOver}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
