import { observer } from 'mobx-react-lite';
import { useCallback } from 'react';
import { useCommands } from '../../app/commands';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Repo } from '../../sync/models';
import { setPinned } from '../../sync/mutations';
import { canPush, issuesForRepo, labelsForRepo, milestonesForRepo, pinnedIssues, repoByName } from '../../sync/selectors';
import { Avatar, StateIcon } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { CommentIcon, MilestoneIcon, PinSlashIcon, PlusIcon, TagIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { IssueList } from './IssueList';
import styles from './IssueList.module.css';

const PinnedIssues = observer(function PinnedIssues({ repo }: { repo: Repo }) {
  const pinned = pinnedIssues(repo.id);
  if (!pinned.length) return null;
  const push = canPush(repo.id);
  return (
    <section className={styles.pinned} aria-label="Pinned issues">
      {pinned.map((i) => {
        const author = store().get('user', i.authorId);
        return (
          <div key={i.id} className={styles.pinnedCard}>
            <div className={styles.pinnedTitle}>
              <StateIcon issue={i} size={16} />
              <Link to={`/${repo.owner}/${repo.name}/issues/${i.number}`} className={styles.pinnedLink}>
                {i.title}
              </Link>
              {push && <IconButton icon={PinSlashIcon} size="sm" label="Unpin" onClick={() => setPinned(i, false)} />}
            </div>
            <div className={styles.pinnedMeta}>
              <span>#{i.number}</span>
              <Avatar user={author} size={14} />
              <span>
                opened <RelativeTime date={i.createdAt} />
              </span>
              <span className={styles.pinnedComments}>
                <CommentIcon size={12} /> {i.comments}
              </span>
            </div>
          </div>
        );
      })}
    </section>
  );
});

export default observer(function IssueListPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(owner, name);
  const repoId = repo?.id;
  const source = useCallback(() => (repoId ? issuesForRepo(repoId).filter((i) => !i.isPr) : []), [repoId]);
  const base = `/${owner}/${name}`;
  const openNew = () => navigate(`${base}/issues/new/choose`);
  useShortcuts('Issue list', {
    'g l': { handler: () => navigate(`${base}/labels`), description: 'Go to labels', group: 'Issues' },
    'g m': { handler: () => navigate(`${base}/milestones`), description: 'Go to milestones', group: 'Issues' },
    'shift+c': { handler: openNew, description: 'New issue (templates)', group: 'Issues' },
  });
  useCommands(
    [
      { id: 'issues.new.full', title: 'New issue from template…', group: 'Issues', icon: PlusIcon, shortcut: '⇧C', run: openNew },
      { id: 'issues.labels', title: 'Go to labels', group: 'Issues', icon: TagIcon, run: () => navigate(`${base}/labels`) },
      { id: 'issues.milestones', title: 'Go to milestones', group: 'Issues', icon: MilestoneIcon, run: () => navigate(`${base}/milestones`) },
    ],
    [base],
  );
  if (!repo) return null; // RepoLayout renders loading / not found
  return (
    <IssueList
      kind="issue"
      repo={repo}
      source={source}
      header={
        <>
          <div className={styles.listHeader}>
            <PinnedIssues repo={repo} />
            <div className={styles.headerLinks}>
              <Link to={`${base}/labels`} className={styles.headerLink}>
                <TagIcon size={16} /> Labels <span className={styles.headerCount}>{labelsForRepo(repo.id).length}</span>
              </Link>
              <Link to={`${base}/milestones`} className={styles.headerLink}>
                <MilestoneIcon size={16} /> Milestones <span className={styles.headerCount}>{milestonesForRepo(repo.id).filter((m) => m.state === 'open').length}</span>
              </Link>
              <span className={styles.headerSpacer} />
              <Button variant="primary" size="sm" onClick={openNew} kbd="⇧C">
                New issue
              </Button>
            </div>
          </div>
        </>
      }
    />
  );
});
