import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { store } from '../../sync';
import type { ID, Issue } from '../../sync/models';
import { applySuggestions } from '../../sync/pullMutations';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Field, Input, Textarea } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import { batchOf, clearBatch, defaultSuggestionMessage, removeFromBatch } from './suggestionBatch';
import styles from './Review.module.css';

/**
 * "Commit suggestion(s)": editable headline and description; the server
 * applies every suggestion in one commit on the head branch with a
 * `Co-authored-by` trailer per suggestion author and resolves the threads.
 */
export const CommitSuggestionsDialog = observer(function CommitSuggestionsDialog({ pr, commentIds, open, onClose }: { pr: Issue; commentIds: readonly ID[]; open: boolean; onClose: () => void }) {
  const n = commentIds.length;
  const [message, setMessage] = useState('');
  const [description, setDescription] = useState('');
  const [busy, setBusy] = useState(false);
  const authors = [...new Set(commentIds.map((id) => store().get('reviewComment', id)?.authorId).filter((a): a is ID => a != null && a !== store().viewerId))];
  const submit = () => {
    setBusy(true);
    applySuggestions(pr, [...commentIds], message.trim() || defaultSuggestionMessage(n), description.trim()).then(
      (res) => {
        for (const id of commentIds) removeFromBatch(pr.id, id);
        if (batchOf(pr.id).length === 0) clearBatch(pr.id);
        toast({ kind: 'success', title: n === 1 ? 'Suggestion committed' : `${n} suggestions committed`, description: res.commit_sha.slice(0, 7) });
        setMessage('');
        setDescription('');
        onClose();
      },
      (e: unknown) => toast({ kind: 'error', title: n === 1 ? 'Couldn’t commit the suggestion' : 'Couldn’t commit the suggestions', description: e instanceof Error ? e.message : undefined }),
    ).finally(() => setBusy(false));
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={n === 1 ? 'Commit suggestion' : `Commit ${n} suggestions`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" loading={busy} disabled={n === 0} onClick={submit}>
            Commit changes
          </Button>
        </>
      }
    >
      <div className={styles.commitForm}>
        <Field label="Commit message" htmlFor="sugg-msg">
          <Input id="sugg-msg" value={message} placeholder={defaultSuggestionMessage(n)} onChange={(e) => setMessage(e.target.value)} autoFocus />
        </Field>
        <Field label="Extended description" htmlFor="sugg-desc">
          <Textarea id="sugg-desc" rows={3} value={description} placeholder="Add an optional extended description…" onChange={(e) => setDescription(e.target.value)} />
        </Field>
        {authors.length > 0 && (
          <p className={styles.subtle}>
            Co-authored by {authors.map((a) => store().get('user', a)?.login ?? 'ghost').join(', ')}.
          </p>
        )}
      </div>
    </Dialog>
  );
});

/** Toolbar button with the batch size; opens the dialog for the whole batch. */
export const SuggestionBatchButton = observer(function SuggestionBatchButton({ pr }: { pr: Issue }) {
  const [open, setOpen] = useState(false);
  const ids = batchOf(pr.id);
  if (ids.length === 0 && !open) return null;
  return (
    <>
      <Button size="sm" variant="primary" onClick={() => setOpen(true)} data-testid="commit-suggestions">
        Commit suggestions ({ids.length})
      </Button>
      <CommitSuggestionsDialog pr={pr} commentIds={ids} open={open} onClose={() => setOpen(false)} />
    </>
  );
});
