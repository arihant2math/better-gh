import { useRef, type KeyboardEvent } from 'react';
import { cx } from '@/ui/Button';
import { joinPath } from './commit';
import styles from './Edit.module.css';

/** Apply typed segments (`a/b/`, `../`) to a directory. */
export function applySegments(dir: string, typed: string): { dir: string; name: string } {
  const parts = dir ? dir.split('/') : [];
  const segs = typed.split('/');
  const name = segs.pop() ?? '';
  for (const s of segs) {
    if (s === '..') parts.pop();
    else if (s && s !== '.') parts.push(s);
  }
  return { dir: parts.join('/'), name };
}

/**
 * GitHub-style path field: `repo / dir / dir / [name]`. Typing "/" moves the
 * typed segment into the breadcrumbs; Backspace at the start of the field
 * pulls the last directory back into it. Crumbs are plain text (no links)
 * so a stray click can't drop unsaved edits.
 */
export function PathInput({
  owner,
  repo,
  refName,
  dir,
  name,
  onChange,
  invalid,
  autoFocus,
}: {
  owner: string;
  repo: string;
  refName: string;
  dir: string;
  name: string;
  onChange: (dir: string, name: string) => void;
  invalid?: boolean;
  autoFocus?: boolean;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const parts = dir ? dir.split('/') : [];

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    const el = e.currentTarget;
    if (e.key === 'Backspace' && el.selectionStart === 0 && el.selectionEnd === 0 && parts.length) {
      e.preventDefault();
      const last = parts[parts.length - 1]!;
      onChange(parts.slice(0, -1).join('/'), last + name);
      requestAnimationFrame(() => ref.current?.setSelectionRange(last.length, last.length));
    }
  };

  return (
    <div className={styles.pathInput}>
      <span className={styles.pathRepo} title={`${owner}/${repo}`}>
        {repo}
      </span>
      {parts.map((p, i) => (
        <span key={i} className={styles.pathSeg}>
          <span className={styles.pathSep}>/</span>
          <span className={styles.pathDir} title={joinPath(...parts.slice(0, i + 1))}>
            {p}
          </span>
        </span>
      ))}
      <span className={styles.pathSep}>/</span>
      <input
        ref={ref}
        className={cx(styles.nameInput, invalid && styles.nameInvalid)}
        value={name}
        placeholder="Name your file…"
        aria-label="File name"
        aria-invalid={invalid || undefined}
        spellCheck={false}
        autoFocus={autoFocus}
        size={Math.max(18, name.length + 2)}
        onKeyDown={onKeyDown}
        onChange={(e) => {
          const v = e.target.value;
          if (!v.includes('/')) return onChange(dir, v);
          const next = applySegments(dir, v);
          onChange(next.dir, next.name);
        }}
      />
      <span className={styles.pathIn}>in</span>
      <code className={styles.branchPill}>{refName}</code>
    </div>
  );
}
