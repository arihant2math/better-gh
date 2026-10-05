import { observer } from 'mobx-react-lite';
import { useEffect, useMemo, useRef, useState } from 'react';
import { session } from '../../app/session';
import { Link, navigate, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { useComputed } from '../../sync/hooks';
import type { Issue, Repo } from '../../sync/models';
import { cmp } from '../../sync/selectors';
import { Avatar, StateIcon } from '../../ui/Badge';
import { Button, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, ChevronDownIcon, InboxIcon, LockIcon, OrganizationIcon, PulseIcon, RepoIcon, StarFillIcon } from '../../ui/icons';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { VirtualList } from '../../ui/VirtualList';
import { InvitationsBanner } from '../invitations/InvitationsBanner';
import { issueHref } from '../issues/IssueRow';
import styles from './DashboardPage.module.css';
import { dayLabel, feedFor, groupFeed, type FeedGroup } from './feed';
import { FeedItem } from './FeedItem';

function greeting(): string {
  const h = new Date().getHours();
  return h < 5 ? 'Good evening' : h < 12 ? 'Good morning' : h < 18 ? 'Good afternoon' : 'Good evening';
}

const byUpdated = (a: Issue, b: Issue) => cmp(b.updatedAt, a.updatedAt);

type WorkTab = 'assigned' | 'review' | 'created' | 'mentioned';
const WORK_TABS: { id: WorkTab; label: string; empty: string; more: string }[] = [
  { id: 'assigned', label: 'Assigned', empty: 'Nothing assigned to you', more: '/issues?q=is%3Aopen+assignee%3A%40me' },
  { id: 'review', label: 'Review requests', empty: 'No pending reviews', more: '/pulls?q=is%3Aopen+review-requested%3A%40me' },
  { id: 'created', label: 'Created', empty: 'Nothing open that you created', more: '/issues?q=is%3Aopen+author%3A%40me' },
  { id: 'mentioned', label: 'Mentioned', empty: 'No open threads mention you', more: '/notifications?reason=mention,team_mention' },
];
const TAB_KEY = 'bgh.dashboard.tab';

function storedTab(): WorkTab {
  try {
    const v = localStorage.getItem(TAB_KEY);
    return WORK_TABS.some((t) => t.id === v) ? (v as WorkTab) : 'assigned';
  } catch {
    return 'assigned';
  }
}

const CompactIssue = observer(function CompactIssue({ issue }: { issue: Issue }) {
  const repo = store().get('repo', issue.repoId);
  const labels = issue.labelIds.slice(0, 2).map((id) => store().get('label', id));
  return (
    <Link to={issueHref(issue)} className={styles.issue}>
      <StateIcon issue={issue} />
      <span className={styles.issueMain}>
        <span className={styles.issueTitle}>{issue.title}</span>
        <span className={styles.issueMeta}>
          {repo?.owner}/{repo?.name}#{issue.number}
          {labels.map((l) => l && <span key={l.id} className={styles.labelDot} style={{ background: `#${l.color}` }} title={l.name} />)}
        </span>
      </span>
      <span className={styles.time}>
        <RelativeTime date={issue.updatedAt} short />
      </span>
    </Link>
  );
});

/** Context switcher: everything, your own repositories, or one organization. */
const ContextSwitcher = observer(function ContextSwitcher({ value }: { value: string }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const me = session.user!;
  const s = store();
  const orgs = s
    .byIndex('membership', 'userId', me.id)
    .map((m) => s.get('org', m.orgId))
    .filter((o): o is NonNullable<typeof o> => !!o);
  // Orgs whose repos you can see even without a membership row.
  for (const o of s.all('org')) if (!orgs.some((x) => x.id === o.id)) orgs.push(o);
  orgs.sort((a, b) => a.login.localeCompare(b.login));
  const current = value ? (orgs.find((o) => o.login.toLowerCase() === value.toLowerCase()) ?? null) : null;
  const label = !value ? 'All activity' : value.toLowerCase() === me.login.toLowerCase() ? me.login : (current?.login ?? value);
  return (
    <>
      <button ref={ref} type="button" className={styles.switcher} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(true)}>
        {value ? <Avatar user={current ?? { login: me.login, avatarUrl: me.avatarUrl }} size={18} square={!!current} /> : <PulseIcon size={16} />}
        <span>{label}</span>
        <ChevronDownIcon size={14} />
      </button>
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        aria-label="Dashboard context"
        items={[
          { header: 'Switch dashboard context', id: 'h' },
          { id: 'all', label: 'All activity', icon: PulseIcon, onSelect: () => setQuery({ ctx: null }) },
          { id: 'me', label: me.login, leading: <Avatar user={{ login: me.login, avatarUrl: me.avatarUrl }} size={16} />, onSelect: () => setQuery({ ctx: me.login }) },
          ...(orgs.length ? [{ separator: true as const, id: 's' }] : []),
          ...orgs.map((o) => ({ id: `o${o.id}`, label: o.login, description: o.name ?? undefined, leading: <Avatar user={o} size={16} square />, onSelect: () => setQuery({ ctx: o.login }) })),
        ]}
      />
    </>
  );
});

const WorkPanel = observer(function WorkPanel({ inCtx }: { inCtx: (r: Repo | undefined) => boolean }) {
  const me = session.user!;
  const s = store();
  const [tab, setTab] = useState<WorkTab>(storedTab);
  const data = useComputed(() => {
    const keep = (i: Issue) => i.state === 'open' && inCtx(s.get('repo', i.repoId));
    const assigned = s.byIndex('issue', 'assigneeIds', me.id).filter(keep).sort(byUpdated);
    const review = s
      .all('issue')
      .filter((i) => i.isPr && keep(i) && i.requestedReviewerIds?.includes(me.id))
      .sort(byUpdated);
    const created = s.byIndex('issue', 'authorId', me.id).filter(keep).sort(byUpdated);
    const seen = new Set<number>();
    const mentioned: Issue[] = [];
    for (const n of s.all('notification')) {
      if ((n.reason !== 'mention' && n.reason !== 'team_mention') || n.subjectId == null || seen.has(n.subjectId)) continue;
      const i = s.get('issue', n.subjectId);
      if (i && keep(i)) {
        seen.add(i.id);
        mentioned.push(i);
      }
    }
    mentioned.sort(byUpdated);
    return { assigned, review, created, mentioned };
  }, [me.id, inCtx]);
  const list = data[tab];
  const def = WORK_TABS.find((t) => t.id === tab)!;
  return (
    <section className={styles.work} aria-label="Your work">
      <div className={styles.workTabs} role="tablist">
        {WORK_TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={t.id === tab}
            className={styles.workTab}
            onClick={() => {
              setTab(t.id);
              try {
                localStorage.setItem(TAB_KEY, t.id);
              } catch {
                /* ignore */
              }
            }}
          >
            {t.label}
            <span className={styles.workCount}>{data[t.id].length}</span>
          </button>
        ))}
      </div>
      {list.length === 0 ? (
        <div className={styles.empty}>
          <CheckCircleIcon size={16} /> {def.empty}
        </div>
      ) : (
        <div className={styles.list}>
          {list.slice(0, 6).map((i) => (
            <CompactIssue key={i.id} issue={i} />
          ))}
          {list.length > 6 && (
            <Link to={def.more} className={styles.viewAll}>
              View all {list.length}
            </Link>
          )}
        </div>
      )}
    </section>
  );
});

type FeedEntry = { kind: 'day'; label: string } | { kind: 'group'; group: FeedGroup } | { kind: 'end' };

/** Home: greeting, your work (local store), activity feed (server, infinite) and recent repositories. */
export default observer(function DashboardPage() {
  const me = session.user!;
  const s = store();
  const ctx = useQuery().get('ctx') ?? '';
  const feed = feedFor(ctx || null);
  const inCtx = useMemo(() => (r: Repo | undefined) => !ctx || (!!r && r.owner.toLowerCase() === ctx.toLowerCase()), [ctx]);

  useEffect(() => {
    void feed.refresh();
  }, [feed]);

  const side = useComputed(() => {
    const repos = s
      .all('repo')
      .filter((r) => inCtx(r))
      .sort((a, b) => cmp(b.pushedAt ?? '', a.pushedAt ?? ''));
    const unread = s.all('notification').filter((n) => n.unread && inCtx(s.get('repo', n.repoId))).length;
    const assigned = s.byIndex('issue', 'assigneeIds', me.id).filter((i) => i.state === 'open' && !i.isPr && inCtx(s.get('repo', i.repoId))).length;
    const reviews = s.all('issue').filter((i) => i.isPr && i.state === 'open' && i.requestedReviewerIds?.includes(me.id) && inCtx(s.get('repo', i.repoId))).length;
    return { repos, unread, assigned, reviews };
  }, [inCtx, me.id]);

  const entries = useMemo<FeedEntry[]>(() => {
    const out: FeedEntry[] = [];
    let day = '';
    for (const g of groupFeed(feed.events)) {
      const label = dayLabel(g.createdAt);
      if (label !== day) {
        out.push({ kind: 'day', label });
        day = label;
      }
      out.push({ kind: 'group', group: g });
    }
    out.push({ kind: 'end' });
    return out;
    // `feed.events` is replaced (not mutated) on every load.
  }, [feed.events]);

  useShortcuts('Home', {
    'g a': { handler: () => setQuery({ ctx: null }), description: 'All activity', group: 'Home' },
    r: { handler: () => void feed.refresh(), description: 'Refresh activity', group: 'Home' },
  });

  if (s.count('repo') === 0) {
    return (
      <>
        <div className={styles.emptyInvites}>
          <InvitationsBanner />
        </div>
        <EmptyState icon={RepoIcon} title="No repositories yet">
          Create a repository or ask to be added to an organization.
        </EmptyState>
      </>
    );
  }

  const header = (
    <div className={styles.feedHeader}>
      <div className={styles.titleRow}>
        <h1 className={styles.greeting}>
          {greeting()}, {me.name?.split(' ')[0] ?? me.login}
        </h1>
        <ContextSwitcher value={ctx} />
      </div>
      <InvitationsBanner />
      <p className={styles.sub}>
        {side.assigned} open issue{side.assigned === 1 ? '' : 's'} assigned to you, {side.reviews} review request{side.reviews === 1 ? '' : 's'}
        {side.unread ? `, ${side.unread} unread notification${side.unread === 1 ? '' : 's'}` : ''}.
      </p>
      <WorkPanel inCtx={inCtx} />
      <h2 className={styles.feedTitle}>Activity</h2>
    </div>
  );

  return (
    <div className={styles.page}>
      <VirtualList
        className={styles.mainCol}
        items={entries}
        estimateSize={64}
        overscan={8}
        header={header}
        aria-label="Activity feed"
        getKey={(e, i) => (e.kind === 'group' ? e.group.key : e.kind === 'day' ? `d:${e.label}` : `end:${i}`)}
        renderItem={(e) =>
          e.kind === 'day' ? (
            <div className={styles.day}>{e.label}</div>
          ) : e.kind === 'group' ? (
            <FeedItem group={e.group} />
          ) : (
            <FeedEnd feed={feed} />
          )
        }
      />
      <aside className={styles.sideCol}>
        <Link to="/notifications" className={styles.inboxCard}>
          <InboxIcon size={18} />
          <span>
            <strong>{side.unread}</strong> unread notification{side.unread === 1 ? '' : 's'}
          </span>
        </Link>
        <section className={styles.section}>
          <header className={styles.sectionHeader}>
            <h2>Recent repositories</h2>
            <span className={styles.count}>{side.repos.length}</span>
          </header>
          <div className={styles.repos}>
            {side.repos.slice(0, 12).map((r) => (
              <Link key={r.id} to={`/${r.owner}/${r.name}`} className={styles.repo}>
                {r.visibility === 'internal' ? <OrganizationIcon size={14} /> : r.private ? <LockIcon size={14} /> : <RepoIcon size={14} />}
                <span className={styles.repoName}>
                  <span className={styles.repoOwner}>{r.owner}/</span>
                  {r.name}
                </span>
                {s.get('viewerRepo', r.id)?.starred && <StarFillIcon size={12} className={styles.star} />}
                {r.pushedAt && (
                  <span className={styles.time}>
                    <RelativeTime date={r.pushedAt} short />
                  </span>
                )}
              </Link>
            ))}
          </div>
        </section>
      </aside>
    </div>
  );
});

/** Last row: loads the next page when it scrolls into view. */
const FeedEnd = observer(function FeedEnd({ feed }: { feed: ReturnType<typeof feedFor> }) {
  useEffect(() => {
    if (!feed.done && !feed.loading && feed.events.length) void feed.loadMore();
  }, [feed, feed.done, feed.loading, feed.events.length]);
  if (feed.error)
    return (
      <div className={styles.feedEnd}>
        {feed.error}{' '}
        <Button size="sm" variant="ghost" onClick={() => (feed.events.length ? feed.loadMore() : feed.refresh())}>
          Retry
        </Button>
      </div>
    );
  if (!feed.events.length && feed.loading)
    return (
      <div className={styles.feedSkeleton}>
        {Array.from({ length: 5 }, (_, i) => (
          <div key={i} className={styles.feedSkeletonRow}>
            <Skeleton width={28} height={28} style={{ borderRadius: '50%' }} />
            <span style={{ flex: 1 }}>
              <Skeleton width="55%" />
              <Skeleton width="80%" style={{ marginTop: 6 }} />
            </span>
          </div>
        ))}
      </div>
    );
  if (!feed.events.length) return <div className={styles.feedEnd}>No activity yet. Star or watch repositories and follow people to fill your feed.</div>;
  if (feed.done) return <div className={styles.feedEnd}>You’re all caught up.</div>;
  return (
    <div className={cx(styles.feedEnd)}>
      <Spinner size={14} /> Loading more…
    </div>
  );
});

export function goToIssue(i: Issue) {
  navigate(issueHref(i));
}
