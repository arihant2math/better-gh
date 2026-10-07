import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { NotFound } from '../../app/NotFound';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import { MarkdownEditor } from '../../components/editor/MarkdownEditor';
import { navigate, useParams } from '../../router';
import { formatKeys } from '../../shortcuts/manager';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import type { Milestone, Repo } from '../../sync/models';
import { createMilestone, deleteMilestone, updateMilestone } from '../../sync/mutations';
import { canPush, milestoneByNumber, milestonesForRepo, repoByName } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { Input } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import { fromDateInput, toDateInput } from './due';
import styles from './Milestones.module.css';

/** `/milestones/new` and `/milestones/:number/edit`. */
export default observer(function MilestoneFormPage() {
  const { owner, repo: name, number } = useParams<{ owner: string; repo: string; number?: string }>();
  const repo = repoByName(owner, name);
  if (!repo) return null;
  if (!canPush(repo.id)) return <NotFound what="page" />;
  const m = number ? milestoneByNumber(repo.id, Number(number)) : undefined;
  if (number && !m) return <NotFound what="milestone" />;
  return <Form key={m?.id ?? 'new'} repo={repo} milestone={m} />;
});

function Form({ repo, milestone }: { repo: Repo; milestone?: Milestone }) {
  const [title, setTitle] = useState(milestone?.title ?? '');
  const [due, setDue] = useState(toDateInput(milestone?.dueOn ?? null));
  const [description, setDescription] = useState(milestone?.description ?? '');
  const [confirm, setConfirm] = useState(false);
  const base = `/${repo.owner}/${repo.name}`;
  const clash = milestonesForRepo(repo.id).find((m) => m.title.toLowerCase() === title.trim().toLowerCase() && m.id !== milestone?.id);
  const valid = title.trim() !== '' && !clash;
  const submit = () => {
    if (!valid) return;
    const input = { title: title.trim(), description: description.trim() || null, dueOn: fromDateInput(due) };
    if (milestone) {
      updateMilestone(milestone, input);
      navigate(`${base}/milestone/${milestone.number}`);
    } else {
      createMilestone(repo, input);
      toast({ kind: 'success', title: `Created milestone ${input.title}` });
      navigate(`${base}/milestones`);
    }
  };
  useShortcuts('Milestone form', { 'mod+enter': { handler: submit, description: 'Save milestone', group: 'Milestone', allowInInput: true } });
  return (
    <div className={styles.formPage}>
      <h1 className={styles.formHeading}>{milestone ? 'Edit milestone' : 'New milestone'}</h1>
      <p className={styles.formHint}>Create a milestone to track progress on groups of issues or pull requests in a repository.</p>
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <label className={styles.field}>
          <span>Title</span>
          <Input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" invalid={!!clash} />
          {clash && <span className={styles.error}>A milestone with this title already exists</span>}
        </label>
        <label className={styles.field}>
          <span>Due date (optional)</span>
          <Input type="date" value={due} onChange={(e) => setDue(e.target.value)} className={styles.date} />
        </label>
        <div className={styles.field}>
          <span>Description</span>
          <MarkdownEditor value={description} onChange={setDescription} repo={`${repo.owner}/${repo.name}`} repoId={repo.id} rows={6} placeholder="Description (optional)" ariaLabel="Description" hideActions />
        </div>
        <div className={styles.formActions}>
          {milestone && (
            <>
              <Button variant="danger" onClick={() => setConfirm(true)}>
                Delete
              </Button>
              <Button
                onClick={() => {
                  updateMilestone(milestone, { state: milestone.state === 'open' ? 'closed' : 'open' });
                  navigate(`${base}/milestones${milestone.state === 'open' ? '?state=closed' : ''}`);
                }}
              >
                {milestone.state === 'open' ? 'Close milestone' : 'Reopen milestone'}
              </Button>
            </>
          )}
          <span className={styles.spacer} />
          <Button variant="ghost" onClick={() => history.back()}>
            Cancel
          </Button>
          <Button type="submit" variant="primary" disabled={!valid} kbd={formatKeys('mod+enter')[0]}>
            {milestone ? 'Save changes' : 'Create milestone'}
          </Button>
        </div>
      </form>
      {milestone && (
        <ConfirmDialog
          open={confirm}
          onClose={() => setConfirm(false)}
          onConfirm={() => {
            deleteMilestone(milestone);
            navigate(`${base}/milestones`, { replace: true });
          }}
          title={`Delete milestone “${milestone.title}”?`}
        >
          Issues and pull requests in this milestone won’t be deleted.
        </ConfirmDialog>
      )}
    </div>
  );
}
