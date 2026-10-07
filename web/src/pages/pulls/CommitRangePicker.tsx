import { useRef, useState } from 'react';
import type { RestCommit } from '../../api/types';
import { Button } from '../../ui/Button';
import { GitCommitIcon, TriangleDownIcon } from '../../ui/icons';
import { Popover } from '../../ui/Popover';
import styles from './Review.module.css';
import { inRange, toggleCommit, type RangeSpec } from './range';

/**
 * Files-tab commit picker: all changes, changes since the viewer's last
 * review, one commit, or a range (shift-click extends the selection).
 */
export function CommitRangePicker({
  spec,
  label,
  commits,
  reviewSha,
  headSha,
  onChange,
}: {
  spec: RangeSpec;
  label: string;
  commits: readonly RestCommit[] | undefined;
  reviewSha: string | null;
  headSha: string | undefined;
  onChange: (spec: RangeSpec) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const pick = (s: RangeSpec) => {
    onChange(s);
    setOpen(false);
  };
  const sinceReview = !!reviewSha && reviewSha !== headSha;
  return (
    <>
      <Button ref={ref} size="sm" className={styles.rangeButton} trailingIcon={TriangleDownIcon} onClick={() => setOpen((o) => !o)} aria-haspopup="listbox" aria-expanded={open} title="Show changes from">
        <span className={styles.rangeLabel}>{label}</span>
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={ref} placement="bottom-start" className={styles.rangePanel}>
        <div role="listbox" aria-label="Commits to show">
          <button type="button" role="option" className={styles.rangeItem} aria-selected={spec.kind === 'all'} onClick={() => pick({ kind: 'all' })}>
            <span className={styles.rangeItemMain}>Show all changes</span>
            {commits && <span className={styles.subtle}>{commits.length} commits</span>}
          </button>
          <button type="button" role="option" className={styles.rangeItem} aria-selected={spec.kind === 'review'} disabled={!sinceReview} onClick={() => pick({ kind: 'review' })}>
            <span className={styles.rangeItemMain}>Show changes since your last review</span>
          </button>
          <div className={styles.rangeSep} />
          <div className={styles.rangeHint}>Select a commit; shift-click to select a range</div>
          {!commits && <div className={styles.rangeHint}>Loading commits…</div>}
          {commits?.map((c) => (
            <button
              key={c.sha}
              type="button"
              role="option"
              className={styles.rangeItem}
              aria-selected={inRange(spec, commits, c.sha)}
              data-sha={c.sha}
              onClick={(e) => {
                const next = toggleCommit(spec, commits, c.sha, e.shiftKey);
                onChange(next);
                if (!e.shiftKey) setOpen(false);
              }}
            >
              <GitCommitIcon size={14} />
              <span className={styles.rangeItemMain}>{c.commit.message.split('\n')[0]}</span>
              <code className={styles.subtle}>{c.sha.slice(0, 7)}</code>
            </button>
          ))}
        </div>
      </Popover>
    </>
  );
}
