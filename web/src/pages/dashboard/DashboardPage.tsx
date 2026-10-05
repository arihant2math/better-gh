import { observer } from 'mobx-react-lite';
import type { ReactNode } from 'react';
import { session } from '../../app/session';
import { Link } from '../../router';
import { store } from '../../sync';
import { useComputed } from '../../sync/hooks';
import type { Issue } from '../../sync/models';
import { cmp } from '../../sync/selectors';
import { StateIcon } from '../../ui/Badge';
import { EmptyState } from '../../ui/EmptyState';
import { CheckCircleIcon, InboxIcon, LockIcon, RepoIcon, StarFillIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { issueHref } from '../issues/IssueRow';
import styles from './DashboardPage.module.css';

function greeting(): string {
  const h = new Date().getHours();
  return h < 5 ? 'Good evening' : h < 12 ? 'Good morning' : h < 18 ? 'Good afternoon' : 'Good evening';
}

const byUpdated = (a: Issue, b: Issue) => cmp(b.updatedAt, a.updatedAt);

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

function Section({ title, count, children, empty, more }: { title: string; count: number; children: ReactNode; empty: string; more?: string }) {
  return (
    <section className={styles.section}>
      <header className={styles.sectionHeader}>
        <h2>{title}</h2>
        <span className={styles.count}>{count}</span>
        {more && count > 0 && (
          <Link to={more} className={styles.more}>
            View all
          </Link>
        )}
      </header>
      {count === 0 ? (
        <div className={styles.empty}>
          <CheckCircleIcon size={16} /> {empty}
        </div>
      ) : (
        <div className={styles.list}>{children}</div>
      )}
    </section>
  );
}

/** Home: everything here is derived from the local store — zero network on navigation. */
export default observer(function DashboardPage() {
  const me = session.user!;
  const s = store();
  const data = useComputed(() => {
    const assigned = s
      .byIndex('issue', 'assigneeIds', me.id)
      .filter((i) => i.state === 'open' && !i.isPr)
      .sort(byUpdated);
    const reviews = s
      .all('issue')
      .filter((i) => i.isPr && i.state === 'open' && i.requestedReviewerIds?.includes(me.id))
      .sort(byUpdated);
    const mine = s
      .byIndex('issue', 'authorId', me.id)
      .filter((i) => i.isPr && i.state === 'open')
      .sort(byUpdated);
    const repos = s
      .all('repo')
      .slice()
      .sort((a, b) => cmp(b.pushedAt ?? '', a.pushedAt ?? ''));
    const unread = s.all('notification').filter((n) => n.unread).length;
    return { assigned, reviews, mine, repos, unread };
  }, [me.id]);

  if (data.repos.length === 0) {
    return <EmptyState icon={RepoIcon} title="No repositories yet">Create a repository or ask to be added to an organization.</EmptyState>;
  }

  return (
    <div className={styles.page}>
      <div className={styles.mainCol}>
        <h1 className={styles.greeting}>
          {greeting()}, {me.name?.split(' ')[0] ?? me.login}
        </h1>
        <p className={styles.sub}>
          {data.assigned.length} open issue{data.assigned.length === 1 ? '' : 's'} assigned to you, {data.reviews.length} review request
          {data.reviews.length === 1 ? '' : 's'}.
        </p>
        <Section title="Review requests" count={data.reviews.length} empty="No pending reviews" more="/pulls?q=is%3Aopen+review-requested%3A%40me">
          {data.reviews.slice(0, 8).map((i) => (
            <CompactIssue key={i.id} issue={i} />
          ))}
        </Section>
        <Section title="Assigned to you" count={data.assigned.length} empty="Nothing assigned to you" more="/issues">
          {data.assigned.slice(0, 10).map((i) => (
            <CompactIssue key={i.id} issue={i} />
          ))}
        </Section>
        <Section title="Your pull requests" count={data.mine.length} empty="No open pull requests" more="/pulls?q=is%3Aopen+author%3A%40me">
          {data.mine.slice(0, 8).map((i) => (
            <CompactIssue key={i.id} issue={i} />
          ))}
        </Section>
      </div>
      <aside className={styles.sideCol}>
        <Link to="/notifications" className={styles.inboxCard}>
          <InboxIcon size={18} />
          <span>
            <strong>{data.unread}</strong> unread notification{data.unread === 1 ? '' : 's'}
          </span>
        </Link>
        <section className={styles.section}>
          <header className={styles.sectionHeader}>
            <h2>Recent repositories</h2>
          </header>
          <div className={styles.repos}>
            {data.repos.slice(0, 10).map((r) => (
              <Link key={r.id} to={`/${r.owner}/${r.name}`} className={styles.repo}>
                {r.private ? <LockIcon size={14} /> : <RepoIcon size={14} />}
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
