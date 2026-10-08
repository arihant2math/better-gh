/**
 * Repository Insights (P31): Pulse, Contributors, Community standards,
 * Commits (commit activity + punch card), Code frequency, Traffic (push
 * access), Forks and Network. One lazy chunk with hand-rolled SVG charts.
 */
import { observer } from 'mobx-react-lite';
import { useMemo, type ReactNode } from 'react';
import { useResource } from '../../../api/cache';
import { listForks } from '../../../api/endpoints';
import {
  getCommunityProfile,
  getPopularPaths,
  getPopularReferrers,
  getStats,
  getTrafficClones,
  getTrafficViews,
  type CodeFrequencyRow,
  type CommunityProfile,
  type ContributorStats,
  type WeekActivity,
} from '../../../api/insights';
import { canPush } from '../../../sync/selectors';
import { useRouteRepo } from '../useRouteRepo';
import type { RestFork } from '../../../api/types';
import { Link, useLocation } from '../../../router';
import { store } from '../../../sync';
import type { Issue, Repo } from '../../../sync/models';
import { Avatar } from '../../../ui/Badge';
import { Box, EmptyState, Skeleton } from '../../../ui/EmptyState';
import { CheckCircleFillIcon, CircleIcon, GitMergeIcon, GitPullRequestIcon, GraphIcon, IssueClosedIcon, IssueOpenedIcon, RepoForkedIcon, RepoIcon } from '../../../ui/icons';
import { RelativeTime } from '../../../ui/RelativeTime';
import { PunchCard, TimeChart } from './charts';
import { authorsSince, commitsSince, compact, formatDay, formatWeek, PULSE_PERIODS, pulsePeriod, rankContributors, weeklyTotals } from './data';
import styles from './Insights.module.css';

type View = 'pulse' | 'contributors' | 'community' | 'commit-activity' | 'code-frequency' | 'traffic' | 'network';

function viewOf(section: string, sub: string | undefined): View | null {
  if (section === 'pulse') return 'pulse';
  if (section === 'community') return 'community';
  if (section === 'network') return 'network';
  if (section === 'graphs' && (sub === 'contributors' || sub === 'commit-activity' || sub === 'code-frequency' || sub === 'traffic')) return sub;
  return null;
}

export default observer(function InsightsPage() {
  const { pathname } = useLocation();
  const repo = useRouteRepo();
  const base = `/${repo.owner}/${repo.name}`;
  const [section = '', sub] = pathname.slice(base.length).split('/').slice(1);
  const view = viewOf(section, sub) ?? 'pulse';
  const pushable = canPush(repo.id);
  const nav: { id: View | 'forks'; label: string; href: string }[] = [
    { id: 'pulse', label: 'Pulse', href: `${base}/pulse` },
    { id: 'contributors', label: 'Contributors', href: `${base}/graphs/contributors` },
    { id: 'community', label: 'Community standards', href: `${base}/community` },
    ...(pushable ? [{ id: 'traffic' as const, label: 'Traffic', href: `${base}/graphs/traffic` }] : []),
    { id: 'commit-activity', label: 'Commits', href: `${base}/graphs/commit-activity` },
    { id: 'code-frequency', label: 'Code frequency', href: `${base}/graphs/code-frequency` },
    { id: 'forks', label: 'Forks', href: `${base}/forks` },
    { id: 'network', label: 'Network', href: `${base}/network/members` },
  ];
  return (
    <div className={styles.page}>
      <nav className={styles.nav} aria-label="Insights">
        {nav.map((n) => (
          <Link key={n.id} to={n.href} className={styles.navItem} aria-current={n.id === view ? 'page' : undefined}>
            {n.label}
          </Link>
        ))}
      </nav>
      <main className={styles.main}>
        {view === 'pulse' && <Pulse repo={repo} period={sub} />}
        {view === 'contributors' && <Contributors repo={repo} />}
        {view === 'community' && <Community repo={repo} />}
        {view === 'commit-activity' && <CommitActivity repo={repo} />}
        {view === 'code-frequency' && <CodeFrequency repo={repo} />}
        {view === 'traffic' && (pushable ? <Traffic repo={repo} /> : <EmptyState icon={GraphIcon} title="Traffic is visible to people with push access" />)}
        {view === 'network' && <Network repo={repo} />}
      </main>
    </div>
  );
});

// ------------------------------------------------------------------ shared

function Header({ title, children }: { title: ReactNode; children?: ReactNode }) {
  return (
    <div className={styles.header}>
      <h2 className={styles.title}>{title}</h2>
      {children}
    </div>
  );
}

function Loading() {
  return (
    <Box padded>
      <Skeleton width="30%" />
      <div style={{ height: 12 }} />
      <Skeleton height={160} />
    </Box>
  );
}

function Failed({ error }: { error: unknown }) {
  return <Box padded>{error instanceof Error ? error.message : 'Could not load this data.'}</Box>;
}

function useStats<T>(repo: Repo, kind: Parameters<typeof getStats>[2]) {
  return useResource<T | null>(`insights:${kind}:${repo.owner}/${repo.name}`.toLowerCase(), () => getStats<T>(repo.owner, repo.name, kind));
}

const EMPTY = <EmptyState icon={GraphIcon} title="No commits yet">Statistics appear once the default branch has commits.</EmptyState>;

// ------------------------------------------------------------------ pulse

const Pulse = observer(function Pulse({ repo, period: periodId }: { repo: Repo; period?: string }) {
  const period = pulsePeriod(periodId);
  const now = Date.now();
  const since = now - period.days * 86_400_000;
  const issues = store().byIndex('issue', 'repoId', repo.id);
  const inPeriod = (t: string | null | undefined) => !!t && Date.parse(t) >= since;
  const merged = issues.filter((i) => i.isPr && i.merged && inPeriod(i.mergedAt));
  const openedPrs = issues.filter((i) => i.isPr && i.state === 'open' && inPeriod(i.createdAt));
  const closedIssues = issues.filter((i) => !i.isPr && i.state === 'closed' && inPeriod(i.closedAt));
  const newIssues = issues.filter((i) => !i.isPr && inPeriod(i.createdAt));
  const activity = useStats<WeekActivity[]>(repo, 'commit_activity');
  const contributors = useStats<ContributorStats[]>(repo, 'contributors');
  const commits = activity.data ? commitsSince(activity.data, since / 1000, now / 1000) : null;
  const authors = contributors.data ? authorsSince(contributors.data, since / 1000, now / 1000) : null;
  const base = `/${repo.owner}/${repo.name}`;
  return (
    <>
      <Header title={`Pulse · ${period.label}`}>
        <nav aria-label="Period" className={styles.legend}>
          {PULSE_PERIODS.map((p) => (
            <Link key={p.id} to={`${base}/pulse/${p.id}`} aria-current={p.id === period.id ? 'page' : undefined} className={styles.navItem}>
              {p.label}
            </Link>
          ))}
        </nav>
      </Header>
      <div className={styles.tiles}>
        <Tile value={merged.length} label="Merged pull requests" />
        <Tile value={openedPrs.length} label="Open pull requests" />
        <Tile value={closedIssues.length} label="Closed issues" />
        <Tile value={newIssues.length} label="New issues" />
      </div>
      <p className={styles.muted}>
        {commits === null || authors === null ? (
          activity.error || contributors.error ? (
            'Commit statistics are unavailable.'
          ) : (
            <Skeleton width={320} />
          )
        ) : (
          <>
            Excluding merges, <strong>{authors.length}</strong> author{authors.length === 1 ? '' : 's'} pushed <strong>{commits}</strong> commit{commits === 1 ? '' : 's'} to {repo.defaultBranch} in the last {period.label}.
          </>
        )}
      </p>
      <IssueList title="Merged pull requests" items={merged} icon={GitMergeIcon} at={(i) => i.mergedAt} base={base} />
      <IssueList title="Opened pull requests" items={openedPrs} icon={GitPullRequestIcon} at={(i) => i.createdAt} base={base} />
      <IssueList title="Closed issues" items={closedIssues} icon={IssueClosedIcon} at={(i) => i.closedAt} base={base} />
      <IssueList title="New issues" items={newIssues} icon={IssueOpenedIcon} at={(i) => i.createdAt} base={base} />
    </>
  );
});

function Tile({ value, label }: { value: ReactNode; label: string }) {
  return (
    <div className={styles.tile}>
      <span className={styles.tileValue}>{value}</span>
      <span className={styles.tileLabel}>{label}</span>
    </div>
  );
}

const IssueList = observer(function IssueList({ title, items, icon: Icon, at, base }: { title: string; items: Issue[]; icon: typeof GraphIcon; at: (i: Issue) => string | null | undefined; base: string }) {
  if (!items.length) return null;
  const sorted = [...items].sort((a, b) => Date.parse(at(b) ?? '') - Date.parse(at(a) ?? '')).slice(0, 10);
  return (
    <section className={styles.section}>
      <h3 className={styles.sectionTitle}>
        {items.length} {title.toLowerCase()}
      </h3>
      <Box>
        <ul className={styles.list}>
          {sorted.map((i) => {
            const author = store().get('user', i.authorId);
            return (
              <li key={i.id} className={styles.row}>
                <Icon size={16} />
                <Link to={`${base}/${i.isPr ? 'pull' : 'issues'}/${i.number}`} className={styles.rowTitle}>
                  {i.title} <span className={styles.muted}>#{i.number}</span>
                </Link>
                <span className={styles.rowMeta}>
                  {author?.login ?? 'ghost'} · <RelativeTime date={at(i) ?? i.createdAt} />
                </span>
              </li>
            );
          })}
        </ul>
      </Box>
    </section>
  );
});

// ------------------------------------------------------------------ contributors

function Contributors({ repo }: { repo: Repo }) {
  const res = useStats<ContributorStats[]>(repo, 'contributors');
  const ranked = useMemo(() => rankContributors(res.data ?? []), [res.data]);
  const totals = useMemo(() => weeklyTotals(res.data ?? []), [res.data]);
  return (
    <>
      <Header title="Contributors">
        <span className={styles.muted}>Commits to {repo.defaultBranch}, excluding merge commits</span>
      </Header>
      {res.error ? (
        <Failed error={res.error} />
      ) : res.data === undefined ? (
        <Loading />
      ) : res.data === null || !ranked.length ? (
        EMPTY
      ) : (
        <>
          <Box padded>
            <TimeChart label="Commits per week" times={totals.map((t) => t.t)} series={[{ label: 'Commits', slot: 1, values: totals.map((t) => t.v) }]} formatTime={formatWeek} />
          </Box>
          <div className={styles.grid2}>
            {ranked.slice(0, 50).map((r, i) => (
              <div key={r.stats.author?.login ?? i} className={styles.card}>
                <div className={styles.cardHead}>
                  {r.stats.author && <Avatar user={{ login: r.stats.author.login, avatarUrl: r.stats.author.avatar_url }} size={24} />}
                  <Link to={`/${r.stats.author?.login ?? ''}`}>{r.stats.author?.login ?? 'ghost'}</Link>
                  <span className={styles.rowMeta}>#{i + 1}</span>
                </div>
                <span className={styles.rowMeta}>
                  {r.commits.toLocaleString()} commit{r.commits === 1 ? '' : 's'} · <span className={styles.add}>{compact(r.additions)} ++</span> · <span className={styles.del}>{compact(r.deletions)} --</span>
                </span>
                <TimeChart
                  label={`Weekly commits by ${r.stats.author?.login ?? 'ghost'}`}
                  height={80}
                  times={r.stats.weeks.map((w) => w.w)}
                  series={[{ label: 'Commits', slot: 1, values: r.stats.weeks.map((w) => w.c) }]}
                  formatTime={formatWeek}
                />
              </div>
            ))}
          </div>
        </>
      )}
    </>
  );
}

// ------------------------------------------------------------------ commits

function CommitActivity({ repo }: { repo: Repo }) {
  const res = useStats<WeekActivity[]>(repo, 'commit_activity');
  const punch = useStats<[number, number, number][]>(repo, 'punch_card');
  return (
    <>
      <Header title="Commit activity">
        <span className={styles.muted}>Commits to {repo.defaultBranch} over the last year</span>
      </Header>
      {res.error ? (
        <Failed error={res.error} />
      ) : res.data === undefined ? (
        <Loading />
      ) : res.data === null ? (
        EMPTY
      ) : (
        <Box padded>
          <TimeChart kind="bar" label="Commits per week, last 52 weeks" times={res.data.map((w) => w.week)} series={[{ label: 'Commits', slot: 1, values: res.data.map((w) => w.total) }]} formatTime={formatWeek} />
        </Box>
      )}
      {punch.data && (
        <section className={styles.section}>
          <h3 className={styles.sectionTitle}>Punch card</h3>
          <Box padded>
            <PunchCard data={punch.data} />
          </Box>
        </section>
      )}
    </>
  );
}

function CodeFrequency({ repo }: { repo: Repo }) {
  const res = useStats<CodeFrequencyRow[]>(repo, 'code_frequency');
  return (
    <>
      <Header title="Code frequency">
        <span className={styles.muted}>Additions and deletions per week</span>
      </Header>
      {res.error ? (
        <Failed error={res.error} />
      ) : res.data === undefined ? (
        <Loading />
      ) : res.data === null || !res.data.length ? (
        EMPTY
      ) : (
        <Box padded>
          <TimeChart
            label="Additions and deletions per week"
            height={240}
            times={res.data.map((r) => r[0])}
            series={[
              { label: 'Additions', slot: 3, values: res.data.map((r) => r[1]) },
              { label: 'Deletions', slot: 2, values: res.data.map((r) => r[2]) },
            ]}
            formatTime={formatWeek}
          />
        </Box>
      )}
    </>
  );
}

// ------------------------------------------------------------------ traffic

function Traffic({ repo }: { repo: Repo }) {
  const key = `${repo.owner}/${repo.name}`.toLowerCase();
  const clones = useResource(`insights:clones:${key}`, () => getTrafficClones(repo.owner, repo.name));
  const views = useResource(`insights:views:${key}`, () => getTrafficViews(repo.owner, repo.name));
  const paths = useResource(`insights:paths:${key}`, () => getPopularPaths(repo.owner, repo.name));
  const referrers = useResource(`insights:referrers:${key}`, () => getPopularReferrers(repo.owner, repo.name));
  const chart = (title: string, data: { count: number; uniques: number; rows: { timestamp: string; count: number; uniques: number }[] } | undefined, error: unknown, unit: string) => (
    <section className={styles.section}>
      <h3 className={styles.sectionTitle}>
        {title}
        {data && (
          <span className={styles.muted}>
            {' '}
            · {data.count.toLocaleString()} {unit} · {data.uniques.toLocaleString()} unique
          </span>
        )}
      </h3>
      {error ? (
        <Failed error={error} />
      ) : !data ? (
        <Loading />
      ) : (
        <Box padded>
          <TimeChart
            kind="bar"
            label={`${title}, last 14 days`}
            times={data.rows.map((r) => Date.parse(r.timestamp) / 1000)}
            series={[
              { label: unit[0]!.toUpperCase() + unit.slice(1), slot: 1, values: data.rows.map((r) => r.count) },
              { label: 'Unique', slot: 2, values: data.rows.map((r) => r.uniques) },
            ]}
            formatTime={(t) => formatDay(new Date(t * 1000).toISOString())}
          />
        </Box>
      )}
    </section>
  );
  return (
    <>
      <Header title="Traffic">
        <span className={styles.muted}>Last 14 days</span>
      </Header>
      {chart('Git clones', clones.data && { ...clones.data, rows: clones.data.clones }, clones.error, 'clones')}
      {chart('Visitors', views.data && { ...views.data, rows: views.data.views }, views.error, 'views')}
      <div className={styles.grid2}>
        <PopularTable
          title="Referring sites"
          head="Site"
          rows={referrers.data?.map((r) => ({ key: r.referrer, label: r.referrer, count: r.count, uniques: r.uniques }))}
        />
        <PopularTable
          title="Popular content"
          head="Content"
          rows={paths.data?.map((p) => ({ key: p.path, label: <Link to={p.path}>{p.path}</Link>, count: p.count, uniques: p.uniques }))}
        />
      </div>
    </>
  );
}

function PopularTable({ title, head, rows }: { title: string; head: string; rows?: { key: string; label: ReactNode; count: number; uniques: number }[] }) {
  return (
    <section className={styles.section}>
      <h3 className={styles.sectionTitle}>{title}</h3>
      <Box>
        {!rows ? (
          <div style={{ padding: 12 }}>
            <Skeleton />
          </div>
        ) : !rows.length ? (
          <div className={styles.check}>
            <span className={styles.muted}>Not enough data yet.</span>
          </div>
        ) : (
          <table className={styles.table}>
            <thead>
              <tr>
                <th>{head}</th>
                <th className={styles.num}>Views</th>
                <th className={styles.num}>Unique</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.key}>
                  <td>{r.label}</td>
                  <td className={styles.num}>{r.count.toLocaleString()}</td>
                  <td className={styles.num}>{r.uniques.toLocaleString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Box>
    </section>
  );
}

// ------------------------------------------------------------------ community

function Community({ repo }: { repo: Repo }) {
  const res = useResource<CommunityProfile>(`insights:community:${repo.owner}/${repo.name}`.toLowerCase(), () => getCommunityProfile(repo.owner, repo.name));
  const p = res.data;
  const base = `/${repo.owner}/${repo.name}`;
  const rows: { label: string; done: boolean; href?: string | null; hint: string }[] = p
    ? [
        { label: 'Description', done: !!p.description, href: `${base}/settings`, hint: 'Describe what the project does.' },
        { label: 'README', done: !!p.files.readme, href: p.files.readme?.html_url, hint: 'Help people get started.' },
        { label: 'Code of conduct', done: !!p.files.code_of_conduct, href: p.files.code_of_conduct_file?.html_url, hint: 'Define community standards.' },
        { label: 'Contributing', done: !!p.files.contributing, href: p.files.contributing?.html_url, hint: 'Explain how to contribute.' },
        { label: 'License', done: !!p.files.license, href: p.files.license?.html_url, hint: 'Let people know what they can do with the code.' },
        { label: 'Security policy', done: !!p.files.security, href: p.files.security?.html_url, hint: 'Explain how to report vulnerabilities (SECURITY.md).' },
        { label: 'Issue templates', done: !!p.files.issue_template, href: p.files.issue_template?.html_url, hint: 'Guide people to file useful issues.' },
        { label: 'Pull request template', done: !!p.files.pull_request_template, href: p.files.pull_request_template?.html_url, hint: 'Guide contributors to useful pull requests.' },
      ]
    : [];
  return (
    <>
      <Header title="Community standards" />
      {res.error ? (
        <Failed error={res.error} />
      ) : !p ? (
        <Loading />
      ) : (
        <>
          <div className={styles.section}>
            <span>
              Health: <strong>{p.health_percentage}%</strong>
            </span>
            <div className={styles.meter} role="meter" aria-label="Community health" aria-valuemin={0} aria-valuemax={100} aria-valuenow={p.health_percentage}>
              <div className={styles.meterFill} style={{ width: `${p.health_percentage}%` }} />
            </div>
          </div>
          <Box>
            <ul className={styles.list} aria-label="Checklist">
              {rows.map((r) => (
                <li key={r.label} className={styles.check}>
                  {r.done ? <CheckCircleFillIcon size={16} className={styles.ok} aria-label="Done" /> : <CircleIcon size={16} className={styles.missing} aria-label="Missing" />}
                  <span className={styles.rowTitle}>
                    {r.done && r.href ? <Link to={r.href.startsWith('http') ? new URL(r.href).pathname : r.href}>{r.label}</Link> : r.label}
                    {!r.done && <span className={styles.muted}> — {r.hint}</span>}
                  </span>
                </li>
              ))}
            </ul>
          </Box>
        </>
      )}
    </>
  );
}

// ------------------------------------------------------------------ network

function Network({ repo }: { repo: Repo }) {
  const res = useResource<{ fork: RestFork; children: RestFork[] }[]>(`insights:network:${repo.owner}/${repo.name}`.toLowerCase(), async () => {
    const forks = await listForks(repo.owner, repo.name);
    // One level of fork-of-fork (bounded), like GitHub's member list.
    const nested = await Promise.all(forks.slice(0, 20).map((f) => (f.forks_count ? listForks(f.owner.login, f.name).catch(() => []) : Promise.resolve([]))));
    return forks.map((fork, i) => ({ fork, children: nested[i] ?? [] }));
  });
  return (
    <>
      <Header title="Network">
        <span className={styles.muted}>Forks of {repo.owner}/{repo.name}</span>
      </Header>
      {res.error ? (
        <Failed error={res.error} />
      ) : !res.data ? (
        <Loading />
      ) : !res.data.length ? (
        <EmptyState icon={RepoForkedIcon} title="No forks yet">
          Forks of this repository will appear here.
        </EmptyState>
      ) : (
        <Box padded>
          <ul className={`${styles.tree} ${styles.treeRoot}`}>
            <li className={styles.treeItem}>
              <RepoIcon size={16} />
              <strong>
                {repo.owner}/{repo.name}
              </strong>
            </li>
            <li>
              <ul className={styles.tree}>
                {res.data.map(({ fork, children }) => (
                  <li key={fork.id}>
                    <ForkItem fork={fork} />
                    {children.length > 0 && (
                      <ul className={styles.tree}>
                        {children.map((c) => (
                          <li key={c.id}>
                            <ForkItem fork={c} />
                          </li>
                        ))}
                      </ul>
                    )}
                  </li>
                ))}
              </ul>
            </li>
          </ul>
        </Box>
      )}
    </>
  );
}

function ForkItem({ fork }: { fork: RestFork }) {
  return (
    <div className={styles.treeItem}>
      <Avatar user={{ login: fork.owner.login, avatarUrl: fork.owner.avatar_url }} size={20} square={fork.owner.type === 'Organization'} />
      <Link to={`/${fork.full_name}`}>{fork.full_name}</Link>
      {fork.pushed_at && (
        <span className={styles.rowMeta}>
          pushed <RelativeTime date={fork.pushed_at} />
        </span>
      )}
    </div>
  );
}
