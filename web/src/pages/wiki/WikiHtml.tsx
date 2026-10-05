import type { MouseEvent } from 'react';
import { navigate, prefetch } from '../../router';
import '../../ui/markdown.css';
import styles from './Wiki.module.css';

/** Server-rendered (sanitized) wiki HTML; internal links go through the router. */
export function WikiHtml({ html, className }: { html: string; className?: string }) {
  const onClick = (e: MouseEvent<HTMLDivElement>) => {
    const a = (e.target as HTMLElement).closest('a');
    if (!a || a.target || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    const href = a.getAttribute('href');
    if (href?.startsWith('/')) {
      e.preventDefault();
      navigate(href);
    }
  };
  const onOver = (e: MouseEvent<HTMLDivElement>) => {
    const href = (e.target as HTMLElement).closest('a')?.getAttribute('href');
    if (href?.startsWith('/')) prefetch(href);
  };
  return (
    <div className={`markdown-body ${styles.html} ${className ?? ''}`} onClick={onClick} onPointerOver={onOver} dangerouslySetInnerHTML={{ __html: html }} />
  );
}
