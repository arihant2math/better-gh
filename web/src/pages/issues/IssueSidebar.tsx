import { observer } from 'mobx-react-lite';
import { useRef, useState, type ReactNode, type RefObject } from 'react';
import { Link } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Issue, Repo } from '../../sync/models';
import { randomLabelColor } from '../../components/labels/colors';
import { addAssignees, addLabels, createLabel, setMilestone, toggleAssignee, toggleLabel } from '../../sync/mutations';
import { assignableUsers, canPush, canWrite, commentsForIssue, labelByName, labelsForRepo, milestonesForRepo } from '../../sync/selectors';
import { Avatar, ColorDot, LabelPill } from '../../ui/Badge';
import { GearIcon, MilestoneIcon } from '../../ui/icons';
import { SelectPanel } from '../../ui/Menu';
import { DevelopmentSection } from './DevelopmentSection';
import styles from './IssueView.module.css';

function Section({
  title,
  children,
  onEdit,
  anchor,
  shortcut,
}: {
  title: string;
  children: ReactNode;
  onEdit?: () => void;
  anchor?: RefObject<HTMLButtonElement | null>;
  shortcut?: string;
}) {
  return (
    <section className={styles.sideSection}>
      {onEdit ? (
        <button ref={anchor} type="button" className={styles.sideHeader} onClick={onEdit} title={shortcut ? `${title} (${shortcut})` : title}>
          {title}
          <GearIcon size={14} />
        </button>
      ) : (
        <div className={styles.sideHeaderStatic}>{title}</div>
      )}
      <div className={styles.sideBody}>{children}</div>
    </section>
  );
}

/** Assignees / labels / milestone pickers. Every toggle is an optimistic mutation. */
export const IssueSidebar = observer(function IssueSidebar({ issue, repo, extra }: { issue: Issue; repo: Repo; extra?: ReactNode }) {
  const s = store();
  const writable = canWrite(repo.id);
  const [open, setOpen] = useState<null | 'assignees' | 'labels' | 'milestone'>(null);
  const aRef = useRef<HTMLButtonElement>(null);
  const lRef = useRef<HTMLButtonElement>(null);
  const mRef = useRef<HTMLButtonElement>(null);

  useShortcuts('Issue', {
    a: { handler: () => (writable ? setOpen('assignees') : false), description: 'Edit assignees', group: 'Issue' },
    l: { handler: () => (writable ? setOpen('labels') : false), description: 'Edit labels', group: 'Issue' },
    m: { handler: () => (writable ? setOpen('milestone') : false), description: 'Set milestone', group: 'Issue' },
  });

  const assignees = issue.assigneeIds.map((id) => s.get('user', id)).filter((u) => u !== undefined);
  const labels = issue.labelIds.map((id) => s.get('label', id)).filter((l) => l !== undefined);
  const milestone = s.get('milestone', issue.milestoneId);
  const participants = new Set<number>([issue.authorId, ...commentsForIssue(issue.id).map((c) => c.authorId)]);

  return (
    <aside className={styles.sidebar} aria-label="Issue details">
      <Section title="Assignees" onEdit={writable ? () => setOpen('assignees') : undefined} anchor={aRef} shortcut="A">
        {assignees.length === 0 ? (
          <span className={styles.subtle}>
            No one —{' '}
            {writable && issue.id > 0 ? (
              <button type="button" className={styles.linkButton} onClick={() => addAssignees(issue, [s.viewerId])}>
                assign yourself
              </button>
            ) : (
              'unassigned'
            )}
          </span>
        ) : (
          assignees.map((u) => (
            <Link key={u.id} to={`/${u.login}`} className={styles.person}>
              <Avatar user={u} size={20} />
              {u.login}
            </Link>
          ))
        )}
      </Section>
      <SelectPanel
        open={open === 'assignees'}
        onClose={() => setOpen(null)}
        anchor={aRef}
        placement="bottom-end"
        title="Assign up to 10 people"
        placeholder="Type or choose a user"
        items={assignableUsers(repo).map((u) => ({
          id: u.id,
          text: u.login,
          description: u.name ?? undefined,
          leading: <Avatar user={u} size={18} />,
          selected: issue.assigneeIds.includes(u.id),
        }))}
        onToggle={(id) => toggleAssignee(issue, Number(id))}
      />

      <Section title="Labels" onEdit={writable ? () => setOpen('labels') : undefined} anchor={lRef} shortcut="L">
        {labels.length === 0 ? (
          <span className={styles.subtle}>None yet</span>
        ) : (
          <div className={styles.labelWrap}>
            {labels.map((l) => (
              <LabelPill key={l.id} label={l} />
            ))}
          </div>
        )}
      </Section>
      <SelectPanel
        open={open === 'labels'}
        onClose={() => setOpen(null)}
        anchor={lRef}
        placement="bottom-end"
        title="Apply labels"
        placeholder="Filter labels"
        items={labelsForRepo(repo.id).map((l) => ({
          id: l.id,
          text: l.name,
          description: l.description ?? undefined,
          leading: <ColorDot color={l.color} />,
          selected: issue.labelIds.includes(l.id),
        }))}
        onToggle={(id) => toggleLabel(issue, Number(id))}
        onCreate={
          canPush(repo.id)
            ? (name) => {
                createLabel(repo, { name, color: randomLabelColor(), description: null });
                const created = labelByName(repo.id, name);
                if (created) addLabels(issue, [created.id]);
              }
            : undefined
        }
        createLabel={(q) => `Create new label “${q}”`}
        footer={<Link to={`/${repo.owner}/${repo.name}/labels`}>Edit labels</Link>}
      />

      <Section title="Milestone" onEdit={writable ? () => setOpen('milestone') : undefined} anchor={mRef} shortcut="M">
        {milestone ? (
          <div className={styles.milestone}>
            <MilestoneIcon size={14} />
            <Link to={`/${repo.owner}/${repo.name}/milestone/${milestone.number}`}>{milestone.title}</Link>
            <progress
              className={styles.progress}
              max={milestone.openIssues + milestone.closedIssues || 1}
              value={milestone.closedIssues}
              aria-label="Milestone progress"
            />
          </div>
        ) : (
          <span className={styles.subtle}>No milestone</span>
        )}
      </Section>
      <SelectPanel
        open={open === 'milestone'}
        onClose={() => setOpen(null)}
        anchor={mRef}
        placement="bottom-end"
        title="Set milestone"
        placeholder="Filter milestones"
        multiple={false}
        emptyText="No milestones in this repository"
        items={[
          ...(issue.milestoneId != null ? [{ id: 'clear', text: 'Clear milestone', selected: false }] : []),
          ...milestonesForRepo(repo.id)
            .filter((m) => m.state === 'open' || m.id === issue.milestoneId)
            .map((m) => ({
              id: m.id,
              text: m.title,
              leading: <MilestoneIcon size={14} />,
              description: m.state === 'closed' ? 'Closed' : m.dueOn ? `Due ${new Date(m.dueOn).toLocaleDateString()}` : 'No due date',
              selected: issue.milestoneId === m.id,
            })),
        ]}
        onToggle={(id) => setMilestone(issue, id === 'clear' || issue.milestoneId === Number(id) ? null : Number(id))}
        footer={<Link to={`/${repo.owner}/${repo.name}/milestones`}>Manage milestones</Link>}
      />

      <DevelopmentSection issue={issue} repo={repo} />

      {extra}

      <Section title={`${participants.size} participant${participants.size === 1 ? '' : 's'}`}>
        <div className={styles.participants}>
          {[...participants].map((id) => (
            <Avatar key={id} user={s.get('user', id)} size={24} title={s.get('user', id)?.login} />
          ))}
        </div>
      </Section>
    </aside>
  );
});
