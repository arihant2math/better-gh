import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Issue } from '../../sync/models';
import { removeReviewers, requestReviewers } from '../../sync/pullMutations';
import { latestReviews, reviewerCandidates } from '../../sync/pullSelectors';
import { canWrite } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { IconButton } from '../../ui/Button';
import { CheckIcon, CommentDiscussionIcon, DotFillIcon, GearIcon, PeopleIcon, SyncIcon, XCircleFillIcon } from '../../ui/icons';
import { SelectPanel } from '../../ui/Menu';
import styles from '../issues/IssueView.module.css';
import pr from './PullDetail.module.css';

/** Sidebar "Reviewers": latest review state per reviewer, pending requests, user/team picker (`R`). */
export const Reviewers = observer(function Reviewers({ issue }: { issue: Issue }) {
  const s = store();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const writable = canWrite(issue.repoId) && issue.state === 'open';
  const latest = latestReviews(issue.id);
  const requested = issue.requestedReviewerIds ?? [];
  const teams = issue.requestedTeamIds ?? [];
  const ids = [...new Set([...requested, ...latest.keys()])];

  useShortcuts('Reviewers', { 'shift+q': { handler: () => writable && setOpen(true), description: 'Request reviewers', group: 'Pull request' } }, writable);

  const { users: cand, teams: candTeams } = open ? reviewerCandidates(issue) : { users: [], teams: [] };

  return (
    <section className={styles.sideSection}>
      {writable ? (
        <button ref={ref} type="button" className={styles.sideHeader} onClick={() => setOpen(true)} title="Request reviewers (Shift+Q)">
          Reviewers
          <GearIcon size={14} />
        </button>
      ) : (
        <div className={styles.sideHeaderStatic}>Reviewers</div>
      )}
      <div className={styles.sideBody}>
        {ids.length === 0 && teams.length === 0 && <span className={styles.subtle}>No reviews</span>}
        {ids.map((id) => {
          const u = s.get('user', id);
          const r = latest.get(id);
          const isRequested = requested.includes(id);
          return (
            <span key={id} className={styles.person}>
              <Avatar user={u} size={20} />
              {u?.login ?? 'ghost'}
              <span className={pr.reviewerState}>
                {isRequested ? (
                  r ? (
                    <span title="Re-requested review">
                      <SyncIcon size={14} className={pr.pending} />
                    </span>
                  ) : (
                    <span title="Awaiting requested review">
                      <DotFillIcon size={16} className={pr.pending} />
                    </span>
                  )
                ) : r?.state === 'APPROVED' ? (
                  <span title="Approved these changes">
                    <CheckIcon size={16} className={pr.ok} />
                  </span>
                ) : r?.state === 'CHANGES_REQUESTED' ? (
                  <span title="Requested changes">
                    <XCircleFillIcon size={16} className={pr.fail} />
                  </span>
                ) : (
                  <span title="Left review comments">
                    <CommentDiscussionIcon size={16} className={pr.muted} />
                  </span>
                )}
                {writable && !isRequested && r && r.state !== 'APPROVED' && (
                  <IconButton icon={SyncIcon} size="sm" label={`Re-request review from ${u?.login}`} onClick={() => requestReviewers(issue, [id])} />
                )}
              </span>
            </span>
          );
        })}
        {teams.map((id) => {
          const t = s.get('team', id);
          return (
            <span key={`t${id}`} className={styles.person}>
              <PeopleIcon size={16} />
              {t ? t.name : `team ${id}`}
              <span className={pr.reviewerState}>
                <DotFillIcon size={16} className={pr.pending} />
              </span>
            </span>
          );
        })}
      </div>
      {open && (
        <SelectPanel
          open
          onClose={() => setOpen(false)}
          anchor={ref}
          placement="bottom-end"
          title="Request up to 15 reviewers"
          placeholder="Type or choose a user or team"
          items={[
            ...candTeams.map((t) => ({ id: `t${t.id}`, text: t.slug, description: t.name, leading: <PeopleIcon size={16} />, selected: teams.includes(t.id) })),
            ...cand.map((u) => ({ id: `u${u.id}`, text: u.login, description: u.name ?? undefined, leading: <Avatar user={u} size={18} />, selected: requested.includes(u.id) })),
          ]}
          onToggle={(raw) => {
            const key = String(raw);
            const id = Number(key.slice(1));
            if (key.startsWith('t')) {
              if (teams.includes(id)) removeReviewers(issue, [], [id]);
              else requestReviewers(issue, [], [id]);
            } else if (requested.includes(id)) removeReviewers(issue, [id]);
            else requestReviewers(issue, [id]);
          }}
        />
      )}
    </section>
  );
});
