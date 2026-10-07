import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useCommands } from '../../app/commands';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import { Link, navigate, setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { useComputed } from '../../sync/hooks';
import type { Milestone, Repo } from '../../sync/models';
import { deleteMilestone, updateMilestone } from '../../sync/mutations';
import { canPush, milestonesForRepo, repoByName } from '../../sync/selectors';
import { Button, cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { CalendarIcon, CheckIcon, MilestoneIcon, PlusIcon } from '../../ui/icons';
import { Select } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { RelativeTime } from '../../ui/RelativeTime';
import { dueText, percentDone } from './due';
import styles from './Milestones.module.css';

type Sort = 'due' | 'due-desc' | 'updated' | 'completeness' | 'title';
const SORTS: { key: Sort; label: string }[] = [
  { key: 'due', label: 'Closest due date' },
  { key: 'due-desc', label: 'Furthest due date' },
  { key: 'updated', label: 'Recently updated' },
  { key: 'completeness', label: 'Least complete' },
  { key: 'title', label: 'Alphabetically' },
];

function sortMilestones(list: Milestone[], sort: Sort): Milestone[] {
  const due = (m: Milestone) => m.dueOn ?? '9999';
  const out = [...list];
  switch (sort) {
    case 'due':
      return out.sort((a, b) => (due(a) < due(b) ? -1 : due(a) > due(b) ? 1 : a.title.localeCompare(b.title)));
    case 'due-desc':
      return out.sort((a, b) => ((a.dueOn ?? '') > (b.dueOn ?? '') ? -1 : (a.dueOn ?? '') < (b.dueOn ?? '') ? 1 : 0));
    case 'updated':
      return out.sort((a, b) => (a.updatedAt > b.updatedAt ? -1 : 1));
    case 'completeness':
      return out.sort((a, b) => percentDone(a) - percentDone(b));
    default:
      return out.sort((a, b) => a.title.localeCompare(b.title));
  }
}

export default observer(function MilestonesPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(owner, name);
  if (!repo) return null;
  return <Milestones repo={repo} />;
});

const Milestones = observer(function Milestones({ repo }: { repo: Repo }) {
  const query = useQuery();
  const state = query.get('state') === 'closed' ? 'closed' : 'open';
  const sort = (query.get('sort') as Sort | null) ?? 'due';
  const writable = canPush(repo.id);
  const base = `/${repo.owner}/${repo.name}`;
  const [active, setActive] = useState(0);
  const all = milestonesForRepo(repo.id);
  const list = useComputed(() => sortMilestones(milestonesForRepo(repo.id).filter((m) => m.state === state), sort), [repo.id, state, sort]);
  const cursor = Math.min(active, Math.max(0, list.length - 1));
  const openNew = () => navigate(`${base}/milestones/new`);

  useShortcuts('Milestones', {
    n: { handler: () => (writable ? openNew() : false), description: 'New milestone', group: 'Milestones' },
    j: { handler: () => setActive(Math.min(list.length - 1, cursor + 1)), description: 'Next milestone', group: 'Milestones' },
    k: { handler: () => setActive(Math.max(0, cursor - 1)), description: 'Previous milestone', group: 'Milestones' },
    enter: { handler: () => (list[cursor] ? navigate(`${base}/milestone/${list[cursor].number}`) : false), description: 'Open milestone', group: 'Milestones' },
    e: { handler: () => (writable && list[cursor] ? navigate(`${base}/milestones/${list[cursor].number}/edit`) : false), description: 'Edit milestone', group: 'Milestones' },
  });
  useCommands(writable ? [{ id: 'milestones.new', title: 'New milestone', group: 'Milestones', icon: PlusIcon, shortcut: 'n', run: openNew }] : [], [writable, base]);

  return (
    <div className={styles.page}>
      <div className={styles.toolbar}>
        <div className={styles.stateTabs} role="tablist">
          <button type="button" role="tab" aria-selected={state === 'open'} className={styles.stateTab} onClick={() => setQuery({ state: null })}>
            <MilestoneIcon size={16} /> {all.filter((m) => m.state === 'open').length} Open
          </button>
          <button type="button" role="tab" aria-selected={state === 'closed'} className={styles.stateTab} onClick={() => setQuery({ state: 'closed' })}>
            <CheckIcon size={16} /> {all.filter((m) => m.state === 'closed').length} Closed
          </button>
        </div>
        <span className={styles.spacer} />
        <Select value={sort} onChange={(e) => setQuery({ sort: e.target.value === 'due' ? null : e.target.value })} aria-label="Sort milestones" className={styles.sort}>
          {SORTS.map((s) => (
            <option key={s.key} value={s.key}>
              Sort: {s.label}
            </option>
          ))}
        </Select>
        {writable && (
          <Button variant="primary" leadingIcon={PlusIcon} kbd="N" onClick={openNew}>
            New milestone
          </Button>
        )}
      </div>
      <div className={styles.box}>
        {list.length === 0 ? (
          <EmptyState icon={MilestoneIcon} title={state === 'open' ? 'No open milestones' : 'No closed milestones'}>
            Milestones group issues and pull requests toward a release or goal.
          </EmptyState>
        ) : (
          <ul className={styles.list} aria-label="Milestones">
            {list.map((m, i) => (
              <MilestoneRow key={m.id} m={m} repo={repo} active={i === cursor} onHover={() => setActive(i)} writable={writable} />
            ))}
          </ul>
        )}
      </div>
    </div>
  );
});

const MilestoneRow = observer(function MilestoneRow({ m, repo, active, onHover, writable }: { m: Milestone; repo: Repo; active: boolean; onHover: () => void; writable: boolean }) {
  const [confirm, setConfirm] = useState(false);
  const base = `/${repo.owner}/${repo.name}`;
  const due = dueText(m);
  const pct = percentDone(m);
  const pending = m.id < 0;
  return (
    <li className={cx(styles.row, active && styles.rowActive)} onMouseEnter={onHover}>
      <div className={styles.rowMain}>
        <Link to={pending ? `${base}/milestones` : `${base}/milestone/${m.number}`} className={styles.rowTitle}>
          {m.title}
        </Link>
        <div className={cx(styles.meta, due.overdue && styles.overdue)}>
          <CalendarIcon size={14} /> {due.text}
          <span className={styles.dotSep}>·</span>
          <span>
            Last updated <RelativeTime date={m.updatedAt} />
          </span>
        </div>
        {m.description && (
          <div className={styles.rowDesc}>
            <Markdown source={m.description} repo={`${repo.owner}/${repo.name}`} />
          </div>
        )}
      </div>
      <div className={styles.rowSide}>
        <progress className={styles.progress} max={100} value={pct} aria-label={`${pct}% complete`} />
        <div className={styles.stats}>
          <span>
            <strong>{pct}%</strong> complete
          </span>
          <Link to={`${base}/issues?q=${encodeURIComponent(`is:open milestone:"${m.title}"`)}`}>
            <strong>{m.openIssues}</strong> open
          </Link>
          <Link to={`${base}/issues?q=${encodeURIComponent(`is:closed milestone:"${m.title}"`)}`}>
            <strong>{m.closedIssues}</strong> closed
          </Link>
        </div>
        {writable && !pending && (
          <div className={styles.rowActions}>
            <Link to={`${base}/milestones/${m.number}/edit`}>Edit</Link>
            <button type="button" className={styles.linkButton} onClick={() => updateMilestone(m, { state: m.state === 'open' ? 'closed' : 'open' })}>
              {m.state === 'open' ? 'Close' : 'Reopen'}
            </button>
            <button type="button" className={cx(styles.linkButton, styles.danger)} onClick={() => setConfirm(true)}>
              Delete
            </button>
          </div>
        )}
      </div>
      <ConfirmDialog open={confirm} onClose={() => setConfirm(false)} onConfirm={() => deleteMilestone(m)} title={`Delete milestone “${m.title}”?`}>
        Issues and pull requests in this milestone won’t be deleted, they’ll just no longer have a milestone.
      </ConfirmDialog>
    </li>
  );
});
