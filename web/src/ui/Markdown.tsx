import { useEffect, useReducer } from 'react';
import { cx } from './Button';
import './markdown.css';
import type * as RenderChunk from './markdown/render';

let chunk: typeof RenderChunk | null = null;
let loading: Promise<void> | null = null;

/** Start loading the markdown chunks (call on intent, e.g. hovering an issue). */
export function preloadMarkdown(): Promise<void> {
  loading ??= Promise.all([import('./markdown/render'), import('./markdown/emoji.json?raw')]).then(([render, emoji]) => {
    render.setEmoji(JSON.parse(emoji.default) as Record<string, string>);
    chunk = render;
  });
  return loading;
}

/**
 * Renders GitHub-flavored markdown (sanitized; `ui/markdown/view.tsx` in
 * the lazy chunk). Until the chunk is loaded it shows the raw text, so
 * content is never blank.
 *
 * With `onSourceChange` (the viewer can edit the body: author or write
 * access), task-list checkboxes are enabled and ticking one calls it with
 * the source rewritten; callers save it through their edit mutation.
 */
export function Markdown(props: { source: string; repo?: string; className?: string; onSourceChange?: (next: string) => void }) {
  const [, force] = useReducer((x: number) => x + 1, 0);
  useEffect(() => {
    if (!chunk) void preloadMarkdown().then(force);
  }, []);
  if (!props.source.trim()) {
    return <div className={cx('markdown-body', 'markdown-empty', props.className)}>No description provided.</div>;
  }
  if (!chunk) {
    return (
      <div className={cx('markdown-body', props.className)}>
        <p style={{ whiteSpace: 'pre-wrap' }}>{props.source}</p>
      </div>
    );
  }
  return <chunk.MarkdownView {...props} />;
}
