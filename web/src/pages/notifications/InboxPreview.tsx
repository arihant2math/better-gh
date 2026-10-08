import { observer } from 'mobx-react-lite';
import { useEffect } from 'react';
import { hasSync, store, sync } from '../../sync';
import { useIssueDetails } from '../../sync/hooks';
import type { Notification } from '../../sync/models';
import { commentsForIssue, reviewsForIssue } from '../../sync/selectors';
import { Avatar, LabelPill, StateBadge } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { BellIcon, BellSlashIcon, CheckIcon, EyeIcon, GitPullRequestIcon, LinkExternalIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { RelativeTime } from '../../ui/RelativeTime';
import { isSubscribed, loadThreadSubscription } from './actions';
import { REASON_LABELS } from './inbox';
import styles from './NotificationsPage.module.css';
import { resettableSet } from '../../api/reset';

const ensured = resettableSet<number>();

interface Props {
  n: Notification;
  onDone: () => void;
  onOpen: () => void;
  onToggleRead: () => void;
  onToggleSubscription: () => void;
  onWatch: () => void;
}

/** Split-view preview: the subject rendered from the local store (body + latest comments load lazily). */
export const InboxPreview = observer(function InboxPreview({ n, onDone, onOpen, onToggleRead, onToggleSubscription, onWatch }: Props) {
  const s = store();
  const issue = n.subjectId != null ? s.get('issue', n.subjectId) : undefined;
  const repo = s.get('repo', n.repoId);
  const loaded = useIssueDetails(issue?.id);

  useEffect(() => {
    loadThreadSubscription(n.id);
    // Subject's repository not synced (e.g. a watched repo you aren't a member of): pull its scope in.
    if (!issue && (n.subjectType === 'Issue' || n.subjectType === 'PullRequest') && hasSync() && !ensured.has(n.repoId)) {
      ensured.add(n.repoId);
      void sync()
        .ensureScope(`repo:${n.repoId}`)
        .catch(() => undefined);
    }
  }, [n.id, n.repoId, n.subjectType, issue]);

  const subscribed = isSubscribed(n.id);
  const actions = (
    <div className={styles.previewActions}>
      <span className={styles.previewRepo}>
        {repo ? `${repo.owner}/${repo.name}` : ''} · {REASON_LABELS[n.reason]} · <RelativeTime date={n.updatedAt} />
      </span>
      <div className={styles.previewButtons}>
        <IconButton icon={EyeIcon} size="sm" label="Watch settings" shortcut="W" onClick={onWatch} disabled={!repo} />
        <IconButton
          icon={subscribed ? BellSlashIcon : BellIcon}
          size="sm"
          label={subscribed ? 'Unsubscribe from this thread' : 'Subscribe to this thread'}
          shortcut="S"
          onClick={onToggleSubscription}
        />
        <Button size="sm" kbd="U" onClick={onToggleRead}>
          {n.unread ? 'Read' : 'Unread'}
        </Button>
        <Button size="sm" leadingIcon={CheckIcon} kbd="E" onClick={onDone}>
          Done
        </Button>
        <Button size="sm" variant="primary" leadingIcon={LinkExternalIcon} kbd="↵" onClick={onOpen}>
          Open
        </Button>
      </div>
    </div>
  );

  if (!issue || !repo) {
    return (
      <div className={styles.preview}>
        {actions}
        <h2 className={styles.previewTitle}>{n.title}</h2>
        {n.subjectType === 'Issue' || n.subjectType === 'PullRequest' ? (
          <div className={styles.previewBody}>
            <Skeleton width="60%" />
            <Skeleton width="85%" style={{ marginTop: 8 }} />
          </div>
        ) : (
          <EmptyState icon={BellIcon} title={n.subjectType === 'Release' ? 'New release' : n.subjectType === 'CheckSuite' ? 'Workflow run' : n.subjectType}>
            Press <kbd>↵</kbd> to open it.
          </EmptyState>
        )}
      </div>
    );
  }

  const author = s.get('user', issue.authorId);
  const comments = loaded ? commentsForIssue(issue.id) : [];
  const recent = comments.slice(-3);
  const reviews = issue.isPr && loaded ? reviewsForIssue(issue.id).filter((r) => r.state !== 'PENDING') : [];
  return (
    <div className={styles.preview}>
      {actions}
      <div className={styles.previewScroll}>
        <h2 className={styles.previewTitle}>
          {issue.title} <span className={styles.previewNumber}>#{issue.number}</span>
        </h2>
        <div className={styles.previewMeta}>
          <StateBadge issue={issue} />
          <span>
            <strong>{author?.login}</strong> opened <RelativeTime date={issue.createdAt} /> · {issue.comments} comment{issue.comments === 1 ? '' : 's'}
          </span>
          {issue.isPr && issue.headRef && (
            <span className={styles.previewBranch}>
              <GitPullRequestIcon size={12} /> {issue.baseRef} ← {issue.headRef}
            </span>
          )}
        </div>
        {issue.labelIds.length > 0 && (
          <div className={styles.previewLabels}>
            {issue.labelIds.map((id) => {
              const l = s.get('label', id);
              return l ? <LabelPill key={id} label={l} size="sm" /> : null;
            })}
          </div>
        )}
        <div className={styles.previewBody}>
          {issue.body === undefined ? (
            <>
              <Skeleton width="90%" />
              <Skeleton width="75%" style={{ marginTop: 8 }} />
              <Skeleton width="80%" style={{ marginTop: 8 }} />
            </>
          ) : issue.body ? (
            <Markdown source={issue.body} repo={`${repo.owner}/${repo.name}`} />
          ) : (
            <p className={styles.muted}>No description provided.</p>
          )}
        </div>
        {reviews.length > 0 && (
          <div className={styles.previewReviews}>
            {reviews.slice(-4).map((r) => {
              const u = s.get('user', r.authorId);
              return (
                <span key={r.id} className={styles.reviewChip} data-state={r.state}>
                  <Avatar user={u} size={16} /> {u?.login} {r.state === 'APPROVED' ? 'approved' : r.state === 'CHANGES_REQUESTED' ? 'requested changes' : 'reviewed'}
                </span>
              );
            })}
          </div>
        )}
        {comments.length > recent.length && (
          <div className={styles.muted} style={{ margin: '8px 0' }}>
            {comments.length - recent.length} earlier comment{comments.length - recent.length === 1 ? '' : 's'}
          </div>
        )}
        {recent.map((c) => {
          const u = s.get('user', c.authorId);
          const isNew = n.lastReadAt == null || c.createdAt > n.lastReadAt;
          return (
            <div key={c.id} className={styles.comment} data-new={isNew || undefined}>
              <div className={styles.commentHeader}>
                <Avatar user={u} size={20} />
                <strong>{u?.login}</strong>
                <RelativeTime date={c.createdAt} />
                {isNew && <span className={styles.newBadge}>New</span>}
              </div>
              <div className={styles.commentBody}>
                <Markdown source={c.body} repo={`${repo.owner}/${repo.name}`} />
              </div>
            </div>
          );
        })}
        {!loaded && issue.comments > 0 && (
          <div className={styles.comment}>
            <Skeleton width="40%" />
            <Skeleton width="90%" style={{ marginTop: 8 }} />
          </div>
        )}
      </div>
    </div>
  );
});
