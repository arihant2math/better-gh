import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import type { Comment, Issue, ReactionContent } from '../../sync/models';
import { toggleReaction } from '../../sync/mutations';
import { viewerReactions } from '../../sync/viewerReactions';
import { cx } from '../../ui/Button';
import { SmileyIcon } from '../../ui/icons';
import { Popover } from '../../ui/Popover';
import styles from './Reactions.module.css';

export const REACTIONS: { content: ReactionContent; emoji: string; label: string }[] = [
  { content: '+1', emoji: '👍', label: 'Thumbs up' },
  { content: '-1', emoji: '👎', label: 'Thumbs down' },
  { content: 'laugh', emoji: '😄', label: 'Laugh' },
  { content: 'hooray', emoji: '🎉', label: 'Hooray' },
  { content: 'confused', emoji: '😕', label: 'Confused' },
  { content: 'heart', emoji: '❤️', label: 'Heart' },
  { content: 'rocket', emoji: '🚀', label: 'Rocket' },
  { content: 'eyes', emoji: '👀', label: 'Eyes' },
];

type Target = { issue: Issue } | { comment: Comment };

/** Picker button: opens the 8 GitHub reactions; each toggles optimistically. */
export const ReactionPicker = observer(function ReactionPicker({ target, disabled }: { target: Target; disabled?: boolean }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const subject = 'issue' in target ? ({ kind: 'issue', id: target.issue.id } as const) : ({ kind: 'comment', id: target.comment.id } as const);
  const mine = viewerReactions(subject);
  return (
    <>
      <button
        ref={ref}
        type="button"
        className={styles.pickerButton}
        aria-label="Add reaction"
        title="Add reaction"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        <SmileyIcon size={16} />
      </button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={ref} placement="top-start" className={styles.picker} role="menu" aria-label="Reactions">
        {REACTIONS.map((r) => (
          <button
            key={r.content}
            type="button"
            role="menuitemcheckbox"
            aria-checked={mine.includes(r.content)}
            aria-label={r.label}
            title={r.label}
            className={cx(styles.pick, mine.includes(r.content) && styles.pickOn)}
            onClick={() => {
              toggleReaction(target, r.content);
              setOpen(false);
            }}
          >
            {r.emoji}
          </button>
        ))}
      </Popover>
    </>
  );
});

/** Reaction pills with counts; the viewer's own are highlighted and toggle on click. */
export const ReactionBar = observer(function ReactionBar({ target, disabled }: { target: Target; disabled?: boolean }) {
  const row = 'issue' in target ? target.issue : target.comment;
  const subject = 'issue' in target ? ({ kind: 'issue', id: row.id } as const) : ({ kind: 'comment', id: row.id } as const);
  const mine = viewerReactions(subject);
  const counts = row.reactions ?? {};
  const shown = REACTIONS.filter((r) => (counts[r.content] ?? 0) > 0);
  const locked = disabled || row.id < 0;
  return (
    <div className={styles.bar}>
      <ReactionPicker target={target} disabled={locked} />
      {shown.map((r) => (
        <button
          key={r.content}
          type="button"
          className={cx(styles.pill, mine.includes(r.content) && styles.pillOn)}
          aria-pressed={mine.includes(r.content)}
          aria-label={`${r.label}: ${counts[r.content]}${mine.includes(r.content) ? ' (you reacted)' : ''}`}
          title={r.label}
          disabled={locked}
          onClick={() => toggleReaction(target, r.content)}
        >
          <span>{r.emoji}</span>
          <span className={styles.count}>{counts[r.content]}</span>
        </button>
      ))}
    </div>
  );
});
