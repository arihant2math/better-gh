import { observer } from 'mobx-react-lite';
import { useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from 'react';
import { formatKeys } from '../../shortcuts/manager';
import { store } from '../../sync';
import type { ID } from '../../sync/models';
import { Avatar, StateIcon } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { fuzzyScore } from '../../ui/fuzzy';
import { Textarea } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { Tabs } from '../../ui/Tabs';
import { caretCoordinates } from './caret';
import { activeToken, continueList, link, prefixLines, wrap, type Edit } from './format';
import styles from './MarkdownEditor.module.css';
import { UploadStatus, useAttachments } from './useAttachments';
import { BoldIcon, CodeIcon, ItalicIcon, LinkIcon, ListOrderedIcon, ListUnorderedIcon, QuoteIcon, TasklistIcon } from '../../ui/icons';

interface Suggestion {
  key: string;
  insert: string;
  label: ReactNode;
  hint?: string;
}

const MAX_SUGGESTIONS = 8;

/** `@` users and `#` issues from the local store, best matches first. */
function suggestions(trigger: '@' | '#', query: string, repoId: ID | undefined): Suggestion[] {
  const s = store();
  if (trigger === '@') {
    const q = query.toLowerCase();
    return s
      .all('user')
      .map((u) => ({ u, score: q ? Math.max(fuzzyScore(q, u.login), u.name ? fuzzyScore(q, u.name) * 0.8 : 0) : 1 }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score || a.u.login.localeCompare(b.u.login))
      .slice(0, MAX_SUGGESTIONS)
      .map(({ u }) => ({
        key: `u${u.id}`,
        insert: `@${u.login} `,
        label: (
          <>
            <Avatar user={u} size={18} />
            <strong>{u.login}</strong>
          </>
        ),
        hint: u.name ?? undefined,
      }));
  }
  if (repoId == null) return [];
  const issues = s.byIndex('issue', 'repoId', repoId).filter((i) => i.id > 0);
  let ranked;
  if (/^\d+$/.test(query)) {
    ranked = issues.filter((i) => String(i.number).startsWith(query)).sort((a, b) => a.number - b.number);
  } else if (query) {
    ranked = issues
      .map((i) => ({ i, score: fuzzyScore(query, i.title) }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score)
      .map((x) => x.i);
  } else {
    ranked = [...issues].sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
  }
  return ranked.slice(0, MAX_SUGGESTIONS).map((i) => ({
    key: `i${i.id}`,
    insert: `#${i.number} `,
    label: (
      <>
        <StateIcon issue={i} size={14} />
        <span className={styles.sugNumber}>#{i.number}</span>
        <span className={styles.sugTitle}>{i.title}</span>
      </>
    ),
  }));
}

/**
 * Markdown editor used for issue bodies, comments and descriptions:
 * Write/Preview, toolbar + shortcuts (⌘B/⌘I/⌘E/⌘K, list continuation),
 * `@mention` and `#issue` autocomplete from the local store, and file
 * attachments (paste, drop or the paperclip; see useAttachments).
 */
export const MarkdownEditor = observer(function MarkdownEditor({
  value,
  onChange,
  repo,
  repoId,
  onSubmit,
  onCancel,
  submitLabel,
  placeholder = 'Leave a comment',
  autoFocus,
  textareaRef,
  extraActions,
  rows = 5,
  submitDisabled,
  ariaLabel = 'Comment body',
  hideActions,
}: {
  value: string;
  onChange: (v: string) => void;
  /** `owner/name` for link rendering in the preview. */
  repo: string;
  /** Enables `#` issue suggestions. */
  repoId?: ID;
  onSubmit?: () => void;
  onCancel?: () => void;
  submitLabel?: string;
  placeholder?: string;
  autoFocus?: boolean;
  textareaRef?: RefObject<HTMLTextAreaElement | null>;
  extraActions?: ReactNode;
  rows?: number;
  submitDisabled?: boolean;
  ariaLabel?: string;
  hideActions?: boolean;
}) {
  const [tab, setTab] = useState('write');
  const ta = useRef<HTMLTextAreaElement | null>(null);
  const setTa = (el: HTMLTextAreaElement | null) => {
    ta.current = el;
    if (textareaRef) textareaRef.current = el;
  };
  const [token, setToken] = useState<ReturnType<typeof activeToken>>(null);
  const [active, setActive] = useState(0);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const pendingSel = useRef<[number, number] | null>(null);

  const items = token ? suggestions(token.trigger, token.query, repoId) : [];
  const open = token !== null && items.length > 0;
  const cursor = Math.min(active, Math.max(0, items.length - 1));

  useLayoutEffect(() => {
    const el = ta.current;
    if (el && pendingSel.current) {
      el.setSelectionRange(...pendingSel.current);
      pendingSel.current = null;
    }
  });

  const apply = (e: Edit) => {
    pendingSel.current = [e.selStart, e.selEnd];
    onChange(e.value);
  };
  const current = (): Edit | null => {
    const el = ta.current;
    return el ? { value: el.value, selStart: el.selectionStart, selEnd: el.selectionEnd } : null;
  };
  const run = (fn: (e: Edit) => Edit) => {
    const e = current();
    if (!e) return;
    apply(fn(e));
    ta.current?.focus();
  };

  const updateToken = () => {
    const el = ta.current;
    if (!el || el.selectionStart !== el.selectionEnd) return setToken(null);
    const t = activeToken(el.value, el.selectionStart);
    setToken((prev) => {
      if (!t) return null;
      if (!prev || prev.start !== t.start || prev.trigger !== t.trigger) setActive(0);
      return t;
    });
    if (t) {
      const c = caretCoordinates(el, t.start);
      setPos({ top: el.offsetTop + c.top + c.height + 4, left: el.offsetLeft + Math.min(c.left, el.clientWidth - 260) });
    }
  };

  const choose = (s: Suggestion | undefined) => {
    const el = ta.current;
    if (!s || !el || !token) return;
    const end = el.selectionStart;
    const v = el.value.slice(0, token.start) + s.insert + el.value.slice(end);
    const at = token.start + s.insert.length;
    apply({ value: v, selStart: at, selEnd: at });
    setToken(null);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (open) {
      if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) {
        e.preventDefault();
        setActive((cursor + 1) % items.length);
        return;
      }
      if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) {
        e.preventDefault();
        setActive((cursor - 1 + items.length) % items.length);
        return;
      }
      if ((e.key === 'Enter' && !e.metaKey && !e.ctrlKey) || e.key === 'Tab') {
        e.preventDefault();
        choose(items[cursor]);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        setToken(null);
        return;
      }
    }
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === 'Enter' && mod) {
      e.preventDefault();
      if (onSubmit && value.trim() && !submitDisabled) onSubmit();
      return;
    }
    if (e.key === 'Escape' && onCancel) {
      e.preventDefault();
      e.stopPropagation();
      onCancel();
      return;
    }
    if (mod && !e.shiftKey && !e.altKey) {
      const k = e.key.toLowerCase();
      const fn = k === 'b' ? (x: Edit) => wrap(x, '**', '**', 'bold') : k === 'i' ? (x: Edit) => wrap(x, '_', '_', 'italic') : k === 'e' ? (x: Edit) => wrap(x, '`', '`', 'code') : k === 'k' ? link : null;
      if (fn) {
        e.preventDefault();
        e.stopPropagation();
        run(fn);
        return;
      }
    }
    if (e.key === 'Enter' && !e.shiftKey && !mod) {
      const cur = current();
      const next = cur && continueList(cur);
      if (next) {
        e.preventDefault();
        apply(next);
      }
    }
  };

  const attachments = useAttachments({ textarea: ta, value, onChange, repo, apply });

  const tools: { label: string; keys?: string; icon: typeof BoldIcon; fn: (e: Edit) => Edit }[] = [
    { label: 'Bold', keys: 'mod+b', icon: BoldIcon, fn: (x) => wrap(x, '**', '**', 'bold') },
    { label: 'Italic', keys: 'mod+i', icon: ItalicIcon, fn: (x) => wrap(x, '_', '_', 'italic') },
    { label: 'Quote', icon: QuoteIcon, fn: (x) => prefixLines(x, '> ') },
    { label: 'Code', keys: 'mod+e', icon: CodeIcon, fn: (x) => (x.value.slice(x.selStart, x.selEnd).includes('\n') ? wrap(x, '```\n', '\n```') : wrap(x, '`', '`', 'code')) },
    { label: 'Link', keys: 'mod+k', icon: LinkIcon, fn: link },
    { label: 'Bulleted list', icon: ListUnorderedIcon, fn: (x) => prefixLines(x, '- ') },
    { label: 'Numbered list', icon: ListOrderedIcon, fn: (x) => prefixLines(x, (i) => `${i + 1}. `) },
    { label: 'Task list', icon: TasklistIcon, fn: (x) => prefixLines(x, '- [ ] ') },
  ];

  return (
    <div className={styles.editor}>
      <div className={styles.tabs}>
        <Tabs
          size="sm"
          value={tab}
          onChange={setTab}
          items={[
            { id: 'write', label: 'Write' },
            { id: 'preview', label: 'Preview' },
          ]}
        />
        {tab === 'write' && (
          <div className={styles.toolbar} role="toolbar" aria-label="Formatting">
            {tools.map((t) => (
              <IconButton
                key={t.label}
                icon={t.icon}
                size="sm"
                label={t.label}
                shortcut={t.keys ? formatKeys(t.keys)[0] : undefined}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => run(t.fn)}
              />
            ))}
            {attachments.button}
          </div>
        )}
      </div>
      {tab === 'write' ? (
        <div className={styles.writeArea}>
          <Textarea
            ref={setTa}
            value={value}
            autoFocus={autoFocus}
            placeholder={placeholder}
            onChange={(e) => {
              onChange(e.target.value);
              requestAnimationFrame(updateToken);
            }}
            onKeyDown={onKeyDown}
            onKeyUp={(e) => (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'Home' || e.key === 'End') && updateToken()}
            onClick={updateToken}
            onBlur={() => setTimeout(() => setToken(null), 120)}
            onPaste={attachments.onPaste}
            onDrop={attachments.onDrop}
            onDragOver={attachments.onDragOver}
            onDragLeave={attachments.onDragLeave}
            rows={rows}
            aria-label={ariaLabel}
            aria-autocomplete="list"
            aria-expanded={open}
            className={cx(styles.textarea, attachments.dragging && styles.dragging)}
          />
          <UploadStatus a={attachments} className={styles.uploadStatus} />
          {open && pos && (
            <div className={styles.suggestions} style={{ top: pos.top, left: Math.max(0, pos.left) }} role="listbox" aria-label={token?.trigger === '@' ? 'Users' : 'Issues'}>
              {items.map((s, i) => (
                <button
                  key={s.key}
                  type="button"
                  role="option"
                  aria-selected={i === cursor}
                  className={cx(styles.suggestion, i === cursor && styles.suggestionActive)}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    choose(s);
                  }}
                  onPointerMove={() => setActive(i)}
                >
                  {s.label}
                  {s.hint && <span className={styles.sugHint}>{s.hint}</span>}
                </button>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div className={styles.preview} style={{ minHeight: rows * 20 + 16 }}>
          <Markdown source={value || '_Nothing to preview_'} repo={repo} />
        </div>
      )}
      {!hideActions && (
        <div className={styles.actions}>
          <span className={styles.hint}>Markdown · @ to mention · # to reference · paste or drop files to attach</span>
          {extraActions}
          <span className={styles.spacer} />
          {onCancel && (
            <Button variant="ghost" onClick={onCancel}>
              Cancel
            </Button>
          )}
          {onSubmit && (
            <Button variant="primary" disabled={!value.trim() || submitDisabled} onClick={onSubmit} kbd={formatKeys('mod+enter')[0]}>
              {submitLabel ?? 'Comment'}
            </Button>
          )}
        </div>
      )}
    </div>
  );
});
