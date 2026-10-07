import { useMemo } from 'react';
import { cx } from '../../../ui/Button';
import { EmptyState } from '../../../ui/EmptyState';
import { FileDiffIcon } from '../../../ui/icons';
import { diffRows } from './lineDiff';
import styles from './Edit.module.css';

const MAX_ROWS = 5000;

/** Unified line diff of the working text against the original. */
export function DiffPreview({ before, after }: { before: string; after: string }) {
  const diff = useMemo(() => diffRows(before, after), [before, after]);
  if (!diff) {
    return (
      <EmptyState icon={FileDiffIcon} title="Too large to diff">
        The changes are too large to preview here. You can still commit them.
      </EmptyState>
    );
  }
  if (!diff.rows.length && !diff.eof) {
    return <EmptyState icon={FileDiffIcon} title="No changes to show" />;
  }
  const rows = diff.rows.slice(0, MAX_ROWS);
  return (
    <div className={styles.diff}>
      <div className={styles.diffStats}>
        <span className={styles.diffAdd}>+{diff.additions}</span>
        <span className={styles.diffDel}>−{diff.deletions}</span>
        {diff.eof && <span className={styles.diffNote}>Newline at end of file {diff.eof}</span>}
      </div>
      <div className={styles.diffScroller}>
        <table className={styles.diffTable}>
          <tbody>
            {rows.map((r, i) =>
              r.kind === 'gap' ? (
                <tr key={i} className={styles.diffGap}>
                  <td colSpan={3}>
                    ⋯ {r.hidden} unchanged line{r.hidden === 1 ? '' : 's'}
                  </td>
                </tr>
              ) : (
                <tr key={i} className={cx(r.kind === 'add' && styles.rowAdd, r.kind === 'del' && styles.rowDel)}>
                  <td className={styles.diffNum}>{r.oldNo ?? ''}</td>
                  <td className={styles.diffNum}>{r.newNo ?? ''}</td>
                  <td className={styles.diffCode}>
                    <span className={styles.diffSign}>{r.kind === 'add' ? '+' : r.kind === 'del' ? '−' : ' '}</span>
                    {r.text}
                  </td>
                </tr>
              ),
            )}
          </tbody>
        </table>
        {diff.rows.length > MAX_ROWS && <div className={styles.diffNote}>Showing the first {MAX_ROWS} rows of the diff.</div>}
      </div>
    </div>
  );
}
