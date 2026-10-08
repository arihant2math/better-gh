import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { store } from '@/sync';
import type { ID, Repo } from '@/sync/models';
import { projectsForOwner, projectsLinkedToRepo } from '@/sync/projects';
import { assignableUsers, labelsForRepo, milestonesForRepo } from '@/sync/selectors';
import { Avatar, ColorDot, LabelPill } from '@/ui/Badge';
import { GearIcon, MilestoneIcon, PeopleIcon, ProjectIcon } from '@/ui/icons';
import { SelectPanel } from '@/ui/Menu';
import styles from './NewIssue.module.css';

const toggle = <T,>(list: T[], id: T) => (list.includes(id) ? list.filter((x) => x !== id) : [...list, id]);

/** Open projects a new issue/PR in `repo` can be added to (linked to the repo, or owned by its owner). */
export function projectCandidates(repo: Repo) {
  const seen = new Set<ID>();
  return [...projectsLinkedToRepo(repo.id), ...projectsForOwner(repo.ownerId)].filter((p) => !p.closed && !seen.has(p.id) && !!seen.add(p.id));
}

/** Reviewer picker state (pull requests only). */
export interface ReviewerPick {
  userIds: ID[];
  setUserIds: (v: ID[]) => void;
  teamIds: ID[];
  setTeamIds: (v: ID[]) => void;
}

/** Project picker state (optional). */
export interface ProjectPick {
  ids: ID[];
  setIds: (v: ID[]) => void;
}

/** Sidebar of the new issue / new pull request forms; values are applied by the caller on submit. */
export const MetaPickers = observer(function MetaPickers({
  repo,
  labelIds,
  setLabelIds,
  assigneeIds,
  setAssigneeIds,
  milestoneId,
  setMilestoneId,
  reviewers,
  projects,
  label = 'Issue metadata',
}: {
  repo: Repo;
  labelIds: ID[];
  setLabelIds: (v: ID[]) => void;
  assigneeIds: ID[];
  setAssigneeIds: (v: ID[]) => void;
  milestoneId: ID | null;
  setMilestoneId: (v: ID | null) => void;
  reviewers?: ReviewerPick;
  projects?: ProjectPick;
  label?: string;
}) {
  const s = store();
  const [open, setOpen] = useState<null | 'r' | 'a' | 'l' | 'p' | 'm'>(null);
  const rRef = useRef<HTMLButtonElement>(null);
  const aRef = useRef<HTMLButtonElement>(null);
  const lRef = useRef<HTMLButtonElement>(null);
  const pRef = useRef<HTMLButtonElement>(null);
  const mRef = useRef<HTMLButtonElement>(null);
  const milestone = s.get('milestone', milestoneId);
  const teams = reviewers && open === 'r' ? s.byIndex('team', 'orgId', repo.ownerId).filter((t) => t.repoIds.includes(repo.id)) : [];
  return (
    <aside className={styles.side} aria-label={label}>
      {reviewers && (
        <>
          <button ref={rRef} type="button" className={styles.sideHeader} onClick={() => setOpen('r')}>
            Reviewers <GearIcon size={14} />
          </button>
          <div className={styles.sideBody}>
            {reviewers.userIds.length + reviewers.teamIds.length === 0 ? (
              <span className={styles.subtle}>No reviews</span>
            ) : (
              <>
                {reviewers.userIds.map((id) => (
                  <span key={`u${id}`} className={styles.person}>
                    <Avatar user={s.get('user', id)} size={20} /> {s.get('user', id)?.login}
                  </span>
                ))}
                {reviewers.teamIds.map((id) => (
                  <span key={`t${id}`} className={styles.person}>
                    <PeopleIcon size={16} /> {s.get('team', id)?.name}
                  </span>
                ))}
              </>
            )}
          </div>
          <SelectPanel
            open={open === 'r'}
            onClose={() => setOpen(null)}
            anchor={rRef}
            placement="bottom-end"
            title="Request up to 15 reviewers"
            items={[
              ...teams.map((t) => ({ id: `t${t.id}`, text: t.name, description: t.description ?? undefined, leading: <PeopleIcon size={16} />, selected: reviewers.teamIds.includes(t.id) })),
              ...assignableUsers(repo)
                .filter((u) => u.id !== s.viewerId)
                .map((u) => ({ id: `u${u.id}`, text: u.login, description: u.name ?? undefined, leading: <Avatar user={u} size={18} />, selected: reviewers.userIds.includes(u.id) })),
            ]}
            onToggle={(key) => {
              const k = String(key);
              const id = Number(k.slice(1));
              if (k.startsWith('t')) reviewers.setTeamIds(toggle(reviewers.teamIds, id));
              else reviewers.setUserIds(toggle(reviewers.userIds, id));
            }}
          />
        </>
      )}

      <button ref={aRef} type="button" className={styles.sideHeader} onClick={() => setOpen('a')}>
        Assignees <GearIcon size={14} />
      </button>
      <div className={styles.sideBody}>
        {assigneeIds.length === 0 ? (
          <span className={styles.subtle}>
            No one —{' '}
            <button type="button" className={styles.linkButton} onClick={() => setAssigneeIds([s.viewerId])}>
              assign yourself
            </button>
          </span>
        ) : (
          assigneeIds.map((id) => (
            <span key={id} className={styles.person}>
              <Avatar user={s.get('user', id)} size={20} /> {s.get('user', id)?.login}
            </span>
          ))
        )}
      </div>
      <SelectPanel
        open={open === 'a'}
        onClose={() => setOpen(null)}
        anchor={aRef}
        placement="bottom-end"
        title="Assign up to 10 people"
        items={assignableUsers(repo).map((u) => ({ id: u.id, text: u.login, description: u.name ?? undefined, leading: <Avatar user={u} size={18} />, selected: assigneeIds.includes(u.id) }))}
        onToggle={(id) => setAssigneeIds(toggle(assigneeIds, Number(id)))}
      />

      <button ref={lRef} type="button" className={styles.sideHeader} onClick={() => setOpen('l')}>
        Labels <GearIcon size={14} />
      </button>
      <div className={styles.sideBody}>
        {labelIds.length === 0 ? (
          <span className={styles.subtle}>None yet</span>
        ) : (
          <div className={styles.labelWrap}>
            {labelIds.map((id) => {
              const l = s.get('label', id);
              return l ? <LabelPill key={id} label={l} /> : null;
            })}
          </div>
        )}
      </div>
      <SelectPanel
        open={open === 'l'}
        onClose={() => setOpen(null)}
        anchor={lRef}
        placement="bottom-end"
        title="Apply labels"
        items={labelsForRepo(repo.id).map((l) => ({ id: l.id, text: l.name, description: l.description ?? undefined, leading: <ColorDot color={l.color} />, selected: labelIds.includes(l.id) }))}
        onToggle={(id) => setLabelIds(toggle(labelIds, Number(id)))}
      />

      {projects && (
        <>
          <button ref={pRef} type="button" className={styles.sideHeader} onClick={() => setOpen('p')}>
            Projects <GearIcon size={14} />
          </button>
          <div className={styles.sideBody}>
            {projects.ids.length === 0 ? (
              <span className={styles.subtle}>None yet</span>
            ) : (
              projects.ids.map((id) => (
                <span key={id} className={styles.person}>
                  <ProjectIcon size={14} /> {s.get('project', id)?.title}
                </span>
              ))
            )}
          </div>
          <SelectPanel
            open={open === 'p'}
            onClose={() => setOpen(null)}
            anchor={pRef}
            placement="bottom-end"
            title="Add to projects"
            emptyText="No open projects"
            items={(open === 'p' ? projectCandidates(repo) : []).map((p) => ({ id: p.id, text: p.title, leading: <ProjectIcon size={14} />, selected: projects.ids.includes(p.id) }))}
            onToggle={(id) => projects.setIds(toggle(projects.ids, Number(id)))}
          />
        </>
      )}

      <button ref={mRef} type="button" className={styles.sideHeader} onClick={() => setOpen('m')}>
        Milestone <GearIcon size={14} />
      </button>
      <div className={styles.sideBody}>
        {milestone ? (
          <span className={styles.person}>
            <MilestoneIcon size={14} /> {milestone.title}
          </span>
        ) : (
          <span className={styles.subtle}>No milestone</span>
        )}
      </div>
      <SelectPanel
        open={open === 'm'}
        onClose={() => setOpen(null)}
        anchor={mRef}
        placement="bottom-end"
        title="Set milestone"
        multiple={false}
        emptyText="No open milestones"
        items={milestonesForRepo(repo.id)
          .filter((m) => m.state === 'open' && m.id > 0)
          .map((m) => ({ id: m.id, text: m.title, leading: <MilestoneIcon size={14} />, selected: milestoneId === m.id }))}
        onToggle={(id) => setMilestoneId(milestoneId === Number(id) ? null : Number(id))}
      />
    </aside>
  );
});
