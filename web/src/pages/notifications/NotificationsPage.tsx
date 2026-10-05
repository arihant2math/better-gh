import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { navigate, prefetch, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { useComputed, useIssueDetails } from '../../sync/hooks';
import type { Notification } from '../../sync/models';
import { markAllNotificationsRead, markNotificationRead, markNotificationUnread } from '../../sync/mutations';
import { notifications } from '../../sync/selectors';
import { LabelPill, StateBadge, StateIcon } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { BellIcon, CheckIcon, DotFillIcon, InboxIcon, LinkExternalIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { VirtualList } from '../../ui/VirtualList';
import { issueHref } from '../issues/IssueRow';
import styles from './NotificationsPage.module.css';

const REASONS: Record<Notification['reason'], string> = {
  assign: 'Assigned',
  author: 'Author',
  comment: 'Comment',
  mention: 'Mentioned',
  review_requested: 'Review requested',
  state_change: 'State change',
  subscribed: 'Subscribed',
  team_mention: 'Team mention',
  manual: 'Subscribed',
  ci_activity: 'CI activity',
  security_alert: 'Security alert',
};

/** Linear-style inbox: list + preview, keyboard driven, optimistic read state. */
export default observer(function NotificationsPage() {
  const filter = useQuery().get('filter') === 'all' ? 'all' : 'unread';
  const items = useComputed(() => notifications().filter((n) => filter === 'all' || n.unread), [filter]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const index = Math.max(0, items.findIndex((n) => n.id === activeId));
  const active = items[index];
  const unreadCount = store()
    .all('notification')
    .filter((n) => n.unread).length;

  const move = (d: number) => {
    const next = items[Math.min(items.length - 1, Math.max(0, index + d))];
    if (next) setActiveId(next.id);
  };
  const open = (n: Notification | undefined) => {
    if (!n?.subjectId) return;
    const issue = store().get('issue', n.subjectId);
    if (n.unread) markNotificationRead(n);
    if (issue) navigate(issueHref(issue));
  };
  const done = (n: Notification | undefined) => {
    if (!n) return;
    // Keep the cursor in place: select the next item before this one disappears from the unread view.
    const next = items[index + 1] ?? items[index - 1];
    if (n.unread) markNotificationRead(n);
    else markNotificationUnread(n);
    if (filter === 'unread' && next) setActiveId(next.id);
  };

  useShortcuts('Inbox', {
    j: { handler: () => move(1), description: 'Next notification', group: 'Inbox' },
    k: { handler: () => move(-1), description: 'Previous notification', group: 'Inbox' },
    arrowdown: { handler: () => move(1), hidden: true },
    arrowup: { handler: () => move(-1), hidden: true },
    enter: { handler: () => open(active), description: 'Open', group: 'Inbox' },
    e: { handler: () => done(active), description: 'Mark read / unread', group: 'Inbox' },
    'shift+e': {
      handler: () => {
        markAllNotificationsRead();
      },
      description: 'Mark all as read', group: 'Inbox' },
  });

  return (
    <div className={styles.page}>
      <div className={styles.listPane}>
        <header className={styles.header}>
          <h1 className={styles.title}>Inbox</h1>
          <Tabs
            size="sm"
            value={filter}
            onChange={(v) => setQuery({ filter: v === 'all' ? 'all' : null })}
            items={[
              { id: 'unread', label: 'Unread', count: unreadCount },
              { id: 'all', label: 'All' },
            ]}
          />
          <span style={{ flex: 1 }} />
          <IconButton icon={CheckIcon} label="Mark all as read" shortcut="⇧E" disabled={unreadCount === 0} onClick={() => markAllNotificationsRead()} />
        </header>
        {items.length === 0 ? (
          <EmptyState icon={InboxIcon} title={filter === 'unread' ? 'Inbox zero' : 'No notifications'}>
            {filter === 'unread' ? 'You’re all caught up. New activity will show up here instantly.' : 'Notifications about issues and pull requests you participate in will show up here.'}
          </EmptyState>
        ) : (
          <VirtualList
            className={styles.list}
            items={items}
            estimateSize={64}
            activeIndex={index}
            getKey={(n) => n.id}
            aria-label="Notifications"
            renderItem={(n) => <Row n={n} active={n === active} onSelect={() => setActiveId(n.id)} onOpen={() => open(n)} />}
          />
        )}
      </div>
      <div className={styles.previewPane}>{active ? <Preview n={active} onDone={() => done(active)} onOpen={() => open(active)} /> : <EmptyState icon={BellIcon} title="Select a notification" />}</div>
    </div>
  );
});

const Row = observer(function Row({ n, active, onSelect, onOpen }: { n: Notification; active: boolean; onSelect: () => void; onOpen: () => void }) {
  const s = store();
  const repo = s.get('repo', n.repoId);
  const issue = s.get('issue', n.subjectId);
  return (
    <div
      className={cx(styles.row, active && styles.rowActive, !n.unread && styles.rowRead)}
      onClick={onSelect}
      onDoubleClick={onOpen}
      onMouseEnter={() => issue && prefetch(issueHref(issue))}
      role="listitem"
    >
      <span className={styles.unreadDot}>{n.unread && <DotFillIcon size={12} />}</span>
      <span className={styles.icon}>{issue ? <StateIcon issue={issue} /> : <BellIcon size={16} />}</span>
      <span className={styles.rowMain}>
        <span className={styles.rowTitle}>{n.title}</span>
        <span className={styles.rowMeta}>
          {repo ? `${repo.owner}/${repo.name}` : ''}
          {issue ? ` #${issue.number}` : ''} · {REASONS[n.reason]}
        </span>
      </span>
      <span className={styles.time}>
        <RelativeTime date={n.updatedAt} short />
      </span>
    </div>
  );
});

const Preview = observer(function Preview({ n, onDone, onOpen }: { n: Notification; onDone: () => void; onOpen: () => void }) {
  const s = store();
  const issue = s.get('issue', n.subjectId);
  const repo = s.get('repo', n.repoId);
  useIssueDetails(issue?.id);
  if (!issue || !repo) return <EmptyState icon={BellIcon} title={n.title} />;
  const author = s.get('user', issue.authorId);
  return (
    <div className={styles.preview}>
      <div className={styles.previewActions}>
        <span className={styles.previewRepo}>
          {repo.owner}/{repo.name} · {REASONS[n.reason]}
        </span>
        <span style={{ flex: 1 }} />
        <Button size="sm" kbd="E" onClick={onDone}>
          {n.unread ? 'Mark as read' : 'Mark as unread'}
        </Button>
        <Button size="sm" variant="primary" leadingIcon={LinkExternalIcon} kbd="↵" onClick={onOpen}>
          Open
        </Button>
      </div>
      <h2 className={styles.previewTitle}>
        {issue.title} <span className={styles.previewNumber}>#{issue.number}</span>
      </h2>
      <div className={styles.previewMeta}>
        <StateBadge issue={issue} />
        <span>
          <strong>{author?.login}</strong> opened <RelativeTime date={issue.createdAt} /> · {issue.comments} comments
        </span>
      </div>
      {issue.labelIds.length > 0 && (
        <div className={styles.previewLabels}>
          {issue.labelIds.map((id) => {
            const l = s.get('label', id);
            return l ? <LabelPill key={id} label={l} /> : null;
          })}
        </div>
      )}
      <div className={styles.previewBody}>
        {issue.body === undefined ? (
          <>
            <Skeleton width="90%" />
            <Skeleton width="75%" style={{ marginTop: 8 }} />
          </>
        ) : (
          <Markdown source={issue.body ?? ''} repo={`${repo.owner}/${repo.name}`} />
        )}
      </div>
    </div>
  );
});
