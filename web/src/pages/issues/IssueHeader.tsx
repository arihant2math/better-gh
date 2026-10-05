import { observer } from 'mobx-react-lite';
import { useState, type ReactNode } from 'react';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Issue } from '../../sync/models';
import { updateIssue } from '../../sync/mutations';
import { canWrite } from '../../sync/selectors';
import { StateBadge } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './IssueView.module.css';

/** Title (inline-editable, optimistic), state badge and the "opened by" line. */
export const IssueHeader = observer(function IssueHeader({ issue, meta }: { issue: Issue; meta?: ReactNode }) {
  const [editing, setEditing] = useState<string | null>(null);
  const author = store().get('user', issue.authorId);
  const writable = canWrite(issue.repoId) || issue.authorId === store().viewerId;

  useShortcuts('Issue', {
    e: { handler: () => (writable && editing === null ? setEditing(issue.title) : false), description: 'Edit title', group: 'Issue' },
  });

  const save = () => {
    const title = (editing ?? '').trim();
    setEditing(null);
    if (title && title !== issue.title) updateIssue(issue, { title });
  };

  return (
    <div className={styles.header}>
      {editing !== null ? (
        <form
          className={styles.titleEdit}
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <Input
            size="lg"
            autoFocus
            value={editing}
            onChange={(e) => setEditing(e.target.value)}
            onKeyDown={(e) => e.key === 'Escape' && setEditing(null)}
            aria-label="Title"
            className={styles.titleInput}
          />
          <Button type="submit" variant="primary">
            Save
          </Button>
          <Button variant="ghost" onClick={() => setEditing(null)}>
            Cancel
          </Button>
        </form>
      ) : (
        <div className={styles.titleRow}>
          <h1 className={styles.title}>
            {issue.title} <span className={styles.number}>#{issue.number > 0 ? issue.number : '…'}</span>
          </h1>
          {writable && (
            <Button size="sm" variant="ghost" kbd="E" onClick={() => setEditing(issue.title)}>
              Edit
            </Button>
          )}
        </div>
      )}
      <div className={styles.metaRow}>
        <StateBadge issue={issue} />
        {meta ?? (
          <span className={styles.metaText}>
            <strong>{author?.login ?? 'ghost'}</strong> opened this {issue.isPr ? 'pull request' : 'issue'} <RelativeTime date={issue.createdAt} /> ·{' '}
            {issue.comments} comment{issue.comments === 1 ? '' : 's'}
          </span>
        )}
      </div>
    </div>
  );
});
