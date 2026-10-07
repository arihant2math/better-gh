import { observer } from 'mobx-react-lite';
import type { MouseEvent } from 'react';
import { arrivals } from '../../app/unread';
import { prefetch } from '../../router';
import { store } from '../../sync';
import type { Notification } from '../../sync/models';
import { Avatar, StateIcon } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { BellIcon, CheckIcon, DotFillIcon, GitCommitIcon, TagIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { issueHref } from '../issues/IssueRow';
import { REASON_LABELS } from './inbox';
import styles from './NotificationsPage.module.css';

const ARRIVAL_MS = 4000;

export interface InboxRowProps {
  n: Notification;
  active: boolean;
  selected: boolean;
  /** Some row is selected: show every checkbox. */
  selecting: boolean;
  /** Callbacks get the row's notification, so the list can pass the same stable functions to every row. */
  onSelect: (n: Notification) => void;
  onToggleSelect: (n: Notification, e: MouseEvent) => void;
  onOpen: (n: Notification) => void;
  onDone: (n: Notification) => void;
  onToggleRead: (n: Notification) => void;
}

export const InboxRow = observer(function InboxRow({ n, active, selected, selecting, onSelect, onToggleSelect, onOpen, onDone, onToggleRead }: InboxRowProps) {
  const s = store();
  const repo = s.get('repo', n.repoId);
  const issue = n.subjectId != null ? s.get('issue', n.subjectId) : undefined;
  const author = issue ? s.get('user', issue.authorId) : undefined;
  const arrivedAt = arrivals.get(n.id);
  const fresh = arrivedAt !== undefined && Date.now() - arrivedAt < ARRIVAL_MS;
  return (
    <div
      className={cx(styles.row, active && styles.rowActive, !n.unread && styles.rowRead, selected && styles.rowSelected, fresh && styles.rowArrived)}
      onClick={(e) => (e.shiftKey || e.metaKey || e.ctrlKey ? onToggleSelect(n, e) : onSelect(n))}
      onDoubleClick={() => onOpen(n)}
      onMouseEnter={() => issue && prefetch(issueHref(issue))}
      role="listitem"
      aria-current={active || undefined}
      data-unread={n.unread || undefined}
      data-id={n.id}
    >
      <span className={cx(styles.check, (selecting || selected) && styles.checkVisible)}>
        <input
          type="checkbox"
          checked={selected}
          aria-label={`Select ${n.title}`}
          onClick={(e) => {
            e.stopPropagation();
            onToggleSelect(n, e);
          }}
          onChange={() => undefined}
        />
      </span>
      <span className={styles.unreadDot}>{n.unread && <DotFillIcon size={12} />}</span>
      <span className={styles.icon}>
        {issue ? (
          <StateIcon issue={issue} />
        ) : n.subjectType === 'Release' ? (
          <TagIcon size={16} />
        ) : n.subjectType === 'Commit' || n.subjectType === 'CheckSuite' ? (
          <GitCommitIcon size={16} />
        ) : (
          <BellIcon size={16} />
        )}
      </span>
      <span className={styles.rowMain}>
        <span className={styles.rowTitle}>{n.title}</span>
        <span className={styles.rowMeta}>
          <span className={styles.rowRepo}>
            {repo ? `${repo.owner}/${repo.name}` : ''}
            {issue ? ` #${issue.number}` : ''}
          </span>
          <span className={styles.reason} data-reason={n.reason}>
            {REASON_LABELS[n.reason]}
          </span>
        </span>
      </span>
      <span className={styles.rowEnd}>
        <span className={styles.rowActions}>
          <button
            type="button"
            className={styles.rowAction}
            title={n.unread ? 'Mark as read (U)' : 'Mark as unread (U)'}
            aria-label={n.unread ? 'Mark as read' : 'Mark as unread'}
            onClick={(e) => {
              e.stopPropagation();
              onToggleRead(n);
            }}
          >
            <DotFillIcon size={12} />
          </button>
          <button
            type="button"
            className={styles.rowAction}
            title="Done (E)"
            aria-label="Mark as done"
            onClick={(e) => {
              e.stopPropagation();
              onDone(n);
            }}
          >
            <CheckIcon size={14} />
          </button>
        </span>
        <span className={styles.rowInfo}>
          {author && <Avatar user={author} size={16} />}
          <span className={styles.time}>
            <RelativeTime date={n.updatedAt} short />
          </span>
        </span>
      </span>
    </div>
  );
});
