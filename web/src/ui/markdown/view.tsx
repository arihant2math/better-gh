/**
 * The rendered view of `ui/Markdown`, part of the lazy markdown chunk so
 * the shell only carries the loader: render cache, repository autolink
 * rules, post-render enhancements, task checkbox and link clicks.
 */
import { useEffect, useReducer, useRef, type MouseEvent as ReactMouseEvent } from 'react';
import { api } from '../../api/client';
import { navigate, prefetch, replaceHash } from '../../router';
import { cx } from '../Button';
import { enhance } from './enhance';
import { renderMarkdown, type RenderContext } from './render';
import type { AutolinkRule } from './scan';
import { countTasks, setTask } from './tasks';
import { resettableMap, sameSession } from '../../api/reset';

const cache = resettableMap<string, string>();

function render(src: string, ctx: RenderContext): string {
  const key = `${ctx.repo ?? ''}\n${ctx.autolinks?.length ?? 0}\n${ctx.tasks ? 1 : 0}\n${src}`;
  let html = cache.get(key);
  if (html === undefined) {
    html = renderMarkdown(src, ctx);
    cache.set(key, html);
    if (cache.size > 600) cache.delete(cache.keys().next().value!);
  }
  return html;
}

/** `GET /_bgh/repos/{o}/{r}/autolinks`, fetched once per repo per session. */
const autolinks = resettableMap<string, readonly AutolinkRule[] | Promise<readonly AutolinkRule[]>>();

/** Rules for `repo` if loaded; otherwise starts loading and calls `onLoad` when there are any. */
function repoAutolinks(repo: string | undefined, onLoad: () => void): readonly AutolinkRule[] | undefined {
  if (!repo || !repo.includes('/')) return undefined;
  let hit = autolinks.get(repo);
  if (Array.isArray(hit)) return hit;
  if (!hit) {
    const [owner, name] = repo.split('/') as [string, string];
    const live = sameSession();
    hit = api
      .get<AutolinkRule[]>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/autolinks`)
      .catch(() => [] as AutolinkRule[])
      .then((rules) => {
        if (live()) autolinks.set(repo, rules);
        return rules;
      });
    autolinks.set(repo, hit);
  }
  void (hit as Promise<readonly AutolinkRule[]>).then((rules) => rules.length && onLoad());
  return undefined;
}

/**
 * Task checkbox and `#anchor` clicks; true when handled. Ticking a box
 * rewrites the nth task in `source` and calls `onSourceChange`.
 */
function handleClick(e: ReactMouseEvent<HTMLElement>, root: HTMLElement, source: string, onSourceChange?: (next: string) => void): boolean {
  const target = e.target as HTMLElement;
  if (target instanceof HTMLInputElement && target.classList.contains('task-list-item-checkbox')) {
    if (!onSourceChange || target.disabled) return true;
    const boxes = [...root.querySelectorAll('input.task-list-item-checkbox')];
    const next = boxes.length === countTasks(source) ? setTask(source, boxes.indexOf(target), target.checked) : null;
    // Rendered and source task lists disagree (e.g. tasks in raw HTML): don't guess.
    if (next === null) e.preventDefault();
    else onSourceChange(next);
    return true;
  }
  const a = target.closest('a');
  const href = a?.getAttribute('href');
  if (!a || a.target || !href?.startsWith('#') || href.length < 2 || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return false;
  // Heading anchors and footnotes: ids carry the `user-content-` prefix.
  const id = decodeURIComponent(href.slice(1));
  const el = document.getElementById(id.startsWith('user-content-') ? id : `user-content-${id}`);
  if (!el) return false;
  e.preventDefault();
  el.scrollIntoView({ block: 'start' });
  replaceHash(href);
  return true;
}

function onPointerOver(e: ReactMouseEvent<HTMLDivElement>) {
  const href = (e.target as HTMLElement).closest('a')?.getAttribute('href');
  if (href?.startsWith('/')) prefetch(href);
}

export interface MarkdownViewProps {
  source: string;
  repo?: string;
  className?: string;
  onSourceChange?: (next: string) => void;
}

export function MarkdownView({ source, repo, className, onSourceChange }: MarkdownViewProps) {
  const [, force] = useReducer((x: number) => x + 1, 0);
  const ref = useRef<HTMLDivElement>(null);
  const html = render(source, { repo, autolinks: repoAutolinks(repo, force), tasks: !!onSourceChange });
  useEffect(() => {
    if (ref.current) enhance(ref.current);
  }, [html]);
  const onClick = (e: ReactMouseEvent<HTMLDivElement>) => {
    if (handleClick(e, e.currentTarget, source, onSourceChange)) return;
    const a = (e.target as HTMLElement).closest('a');
    if (!a || a.target || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    const href = a.getAttribute('href');
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
