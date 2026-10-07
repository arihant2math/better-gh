import { observer } from 'mobx-react-lite';
import { useCallback } from 'react';
import { NotFound } from '../../app/NotFound';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { issuesForRepo, canPush, milestoneByNumber, repoByName } from '../../sync/selectors';
import { updateMilestone } from '../../sync/mutations';
import { Button, cx } from '../../ui/Button';
import { CalendarIcon, MilestoneIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { IssueList } from '../issues/IssueList';
import { dueText, percentDone } from './due';
import styles from './Milestones.module.css';

/** Milestone detail: header with progress, then its issues and PRs (filterable list). */
export default observer(function MilestonePage() {
  const { owner, repo: name, number } = useParams<{ owner: string; repo: string; number: string }>();
  const repo = repoByName(owner, name);
  const m = repo ? milestoneByNumber(repo.id, Number(number)) : undefined;
  const mid = m?.id;
  const repoId = repo?.id;
  const source = useCallback(() => (repoId && mid ? issuesForRepo(repoId).filter((i) => i.milestoneId === mid) : []), [repoId, mid]);
  const base = `/${owner}/${name}`;
  useShortcuts('Milestone', {
    e: { handler: () => (m && repo && canPush(repo.id) ? navigate(`${base}/milestones/${m.number}/edit`) : false), description: 'Edit milestone', group: 'Milestone' },
  });
  if (!repo) return null;
  if (!m) return <NotFound what="milestone" />;
  const due = dueText(m);
  const pct = percentDone(m);
  const writable = canPush(repo.id);
  return (
    <IssueList
      kind="issue"
      repo={repo}
      source={source}
      emptyTitle="No issues in this milestone match"
      header={
        <div className={styles.detailHeader}>
          <div className={styles.detailTop}>
            <Link to={`${base}/milestones`} className={styles.crumb}>
              <MilestoneIcon size={16} /> Milestones
            </Link>
            <span className={styles.spacer} />
            {writable && (
              <>
                <Button size="sm" onClick={() => navigate(`${base}/milestones/${m.number}/edit`)} kbd="E">
                  Edit milestone
                </Button>
                <Button size="sm" onClick={() => updateMilestone(m, { state: m.state === 'open' ? 'closed' : 'open' })}>
                  {m.state === 'open' ? 'Close milestone' : 'Reopen milestone'}
                </Button>
              </>
            )}
          </div>
          <h1 className={styles.detailTitle}>
            {m.title} {m.state === 'closed' && <span className={styles.closedTag}>Closed</span>}
          </h1>
          <div className={cx(styles.meta, due.overdue && styles.overdue)}>
            <CalendarIcon size={14} /> {due.text}
            <span className={styles.dotSep}>·</span>
            <strong>{pct}%</strong>&nbsp;complete · {m.openIssues} open · {m.closedIssues} closed
          </div>
          <progress className={styles.progressWide} max={100} value={pct} aria-label={`${pct}% complete`} />
          {m.description && (
            <div className={styles.detailDesc}>
              <Markdown source={m.description} repo={`${repo.owner}/${repo.name}`} />
            </div>
          )}
        </div>
      }
    />
  );
});
