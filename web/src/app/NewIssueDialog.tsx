import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { navigate } from '../router';
import { store } from '../sync';
import { createIssue } from '../sync/mutations';
import { Button } from '../ui/Button';
import { Dialog } from '../ui/Dialog';
import { Input, Textarea } from '../ui/Input';
import { toast } from '../ui/Toast';
import { formatKeys } from '../shortcuts/manager';
import { ui } from './uiState';

/** Optimistic "create issue": the row appears in lists instantly; we jump to it once numbered. */
export const NewIssueDialog = observer(function NewIssueDialog() {
  const repo = store().get('repo', ui.newIssueRepoId);
  return (
    <Dialog open={!!repo} onClose={() => ui.closeNewIssue()} title={repo ? `New issue in ${repo.owner}/${repo.name}` : ''} position="top">
      {repo && <Form repoId={repo.id} />}
    </Dialog>
  );
});

function Form({ repoId }: { repoId: number }) {
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const submit = () => {
    const repo = store().get('repo', repoId);
    if (!repo || !title.trim()) return;
    const { done } = createIssue(repo, { title: title.trim(), body });
    ui.closeNewIssue();
    done.then(
      (res) => {
        const number = (res.data as { number?: number } | undefined)?.number;
        toast({
          kind: 'success',
          title: `Created issue${number ? ` #${number}` : ''}`,
          action: number ? { label: 'Open', onClick: () => navigate(`/${repo.owner}/${repo.name}/issues/${number}`) } : undefined,
        });
      },
      () => undefined,
    );
  };
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
          e.preventDefault();
          submit();
        }
      }}
      style={{ display: 'flex', flexDirection: 'column', gap: 10 }}
    >
      <Input size="lg" autoFocus placeholder="Issue title" value={title} onChange={(e) => setTitle(e.target.value)} aria-label="Title" />
      <Textarea placeholder="Add a description… (markdown supported)" value={body} onChange={(e) => setBody(e.target.value)} rows={8} aria-label="Description" />
      <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
        <Button variant="ghost" onClick={() => ui.closeNewIssue()}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" disabled={!title.trim()} kbd={formatKeys('mod+enter')[0]}>
          Create issue
        </Button>
      </div>
    </form>
  );
}
