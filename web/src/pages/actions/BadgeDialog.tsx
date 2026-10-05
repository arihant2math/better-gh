import { useState } from 'react';
import type { Workflow } from '../../api/actions';
import { useResource } from '../../api/cache';
import { api } from '../../api/client';
import { listBranches } from '../../api/endpoints';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Field, Select } from '../../ui/Input';
import { CopyIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { EVENTS, workflowFile } from './shared';
import styles from './Runs.module.css';

/** Path of a workflow's status badge (`/{o}/{r}/actions/workflows/{file}/badge.svg`). */
export function badgePath(owner: string, repo: string, workflow: Workflow, branch?: string, event?: string): string {
  const q = new URLSearchParams();
  if (branch) q.set('branch', branch);
  if (event) q.set('event', event);
  const qs = q.toString();
  return `/${owner}/${repo}/actions/workflows/${encodeURIComponent(workflowFile(workflow.path))}/badge.svg${qs ? `?${qs}` : ''}`;
}

/** README Markdown embedding the badge, linking to the workflow's runs. */
export function badgeMarkdown(origin: string, owner: string, repo: string, workflow: Workflow, branch?: string, event?: string): string {
  const runs = `${origin}/${owner}/${repo}/actions/workflows/${encodeURIComponent(workflowFile(workflow.path))}`;
  return `[![${workflow.name}](${origin}${badgePath(owner, repo, workflow, branch, event)})](${runs})`;
}

/** "Create status badge": pick branch / event, preview, copy the Markdown. */
export function BadgeDialog({
  owner,
  repo,
  workflow,
  defaultBranch,
  open,
  onClose,
}: {
  owner: string;
  repo: string;
  workflow: Workflow;
  defaultBranch: string;
  open: boolean;
  onClose: () => void;
}) {
  const [branch, setBranch] = useState('');
  const [event, setEvent] = useState('');
  const branches = useResource(open ? `branches:${owner}/${repo}` : null, () => listBranches(owner, repo));
  const path = badgePath(owner, repo, workflow, branch || undefined, event || undefined);
  // Fetched (not an <img src>) so the preview also works against the mock backend.
  const svg = useResource(open ? `badge:${path}` : null, () => api.get<string>(path, { accept: 'image/svg+xml', text: true }));
  const markdown = badgeMarkdown(window.location.origin, owner, repo, workflow, branch || undefined, event || undefined);
  const copy = () =>
    navigator.clipboard.writeText(markdown).then(
      () => toast({ kind: 'success', title: 'Copied status badge Markdown' }),
      () => toast({ kind: 'error', title: 'Couldn’t copy to the clipboard' }),
    );
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Create status badge"
      footer={
        <Button variant="primary" leadingIcon={CopyIcon} onClick={copy}>
          Copy status badge Markdown
        </Button>
      }
    >
      <div className={styles.badgeForm}>
        <div className={styles.badgePreview} aria-live="polite">
          {svg.data ? <img alt={`${workflow.name} status`} src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg.data)}`} /> : <span className={styles.subtle}>{svg.error ? 'Badge unavailable' : 'Loading…'}</span>}
        </div>
        <Field label="Branch" htmlFor="badge-branch">
          <Select id="badge-branch" value={branch} onChange={(e) => setBranch(e.target.value)}>
            <option value="">Default branch ({defaultBranch})</option>
            {(branches.data ?? [])
              .filter((b) => b.name !== defaultBranch)
              .map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                </option>
              ))}
          </Select>
        </Field>
        <Field label="Event" htmlFor="badge-event">
          <Select id="badge-event" value={event} onChange={(e) => setEvent(e.target.value)}>
            <option value="">Any event</option>
            {EVENTS.map((e) => (
              <option key={e} value={e}>
                {e}
              </option>
            ))}
          </Select>
        </Field>
        <textarea className={styles.badgeMarkdown} readOnly value={markdown} aria-label="Status badge Markdown" rows={3} onFocus={(e) => e.currentTarget.select()} />
      </div>
    </Dialog>
  );
}
