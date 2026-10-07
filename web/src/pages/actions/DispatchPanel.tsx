import { useRef, useState, type FormEvent } from 'react';
import { actionsKey, dispatchWorkflow, getDispatchForm, listEnvironments, type DispatchInput, type Workflow } from '../../api/actions';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { listBranches } from '../../api/endpoints';
import { navigate } from '../../router';
import { Button } from '../../ui/Button';
import { Field, Input, Select } from '../../ui/Input';
import { ChevronDownIcon, PlayIcon } from '../../ui/icons';
import { Popover } from '../../ui/Popover';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import styles from './Runs.module.css';

/** Initial form value of an input (GitHub UI semantics: booleans default to false). */
function initialValue(i: DispatchInput): string {
  if (i.type === 'boolean') return i.default === 'true' ? 'true' : 'false';
  if (i.type === 'choice') return i.default ?? i.options[0] ?? '';
  return i.default ?? '';
}

/** "Run workflow" button + popover for workflows with a `workflow_dispatch` trigger. */
export function DispatchButton({ owner, repo, workflow, defaultBranch }: { owner: string; repo: string; workflow: Workflow; defaultBranch: string }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  // Probe the default branch: only dispatchable workflows get a button.
  const probe = useResource(actionsKey(owner, repo, 'dispatch', workflow.id, defaultBranch), () => getDispatchForm(owner, repo, workflow.id, defaultBranch));
  if (!probe.data?.dispatchable || workflow.state !== 'active') return null;
  return (
    <>
      <Button ref={anchor} size="sm" variant="primary" leadingIcon={PlayIcon} trailingIcon={ChevronDownIcon} aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        Run workflow
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={anchor} placement="bottom-end" className={styles.dispatch} role="dialog" aria-label="Run workflow">
        {open && <DispatchForm owner={owner} repo={repo} workflow={workflow} defaultBranch={defaultBranch} onDone={() => setOpen(false)} />}
      </Popover>
    </>
  );
}

function DispatchForm({ owner, repo, workflow, defaultBranch, onDone }: { owner: string; repo: string; workflow: Workflow; defaultBranch: string; onDone: () => void }) {
  const [ref, setRef] = useState(defaultBranch);
  const branches = useResource(`branches:${owner}/${repo}`, () => listBranches(owner, repo));
  const form = useResource(actionsKey(owner, repo, 'dispatch', workflow.id, ref), () => getDispatchForm(owner, repo, workflow.id, ref));
  const hasEnvInput = !!form.data?.inputs.some((i) => i.type === 'environment');
  const envs = useResource(hasEnvInput ? actionsKey(owner, repo, 'environments') : null, () => listEnvironments(owner, repo));
  // Values typed by the user, per input name; untouched inputs use their defaults.
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const inputs = form.data?.inputs ?? [];
  const valueOf = (i: DispatchInput) => values[i.name] ?? initialValue(i);
  const missing = inputs.filter((i) => i.required && i.type !== 'boolean' && !valueOf(i).trim());

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!form.data?.dispatchable || missing.length) return;
    setBusy(true);
    setError(null);
    try {
      const body: Record<string, string> = {};
      for (const i of inputs) body[i.name] = valueOf(i);
      const res = await dispatchWorkflow(owner, repo, workflow.id, ref, body);
      toast({ kind: 'success', title: `${workflow.name} started on ${ref}` });
      onDone();
      navigate(`/${owner}/${repo}/actions/runs/${res.workflow_run_id}`);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not start the workflow');
    } finally {
      setBusy(false);
    }
  };

  const branchNames = branches.data?.map((b) => b.name) ?? [defaultBranch];
  if (!branchNames.includes(ref)) branchNames.unshift(ref);

  return (
    <form className={styles.dispatchForm} onSubmit={(e) => void submit(e)}>
      <div className={styles.dispatchTitle}>Run workflow</div>
      <Field label="Use workflow from" htmlFor="dispatch-ref">
        <Select id="dispatch-ref" value={ref} onChange={(e) => setRef(e.target.value)}>
          {branchNames.map((b) => (
            <option key={b} value={b}>
              Branch: {b}
            </option>
          ))}
        </Select>
      </Field>
      {form.loading && !form.data ? (
        <div className={styles.dispatchLoading}>
          <Spinner />
        </div>
      ) : form.data && !form.data.dispatchable ? (
        <div className={styles.formError}>{form.data.error ?? 'This workflow cannot be run manually on this branch.'}</div>
      ) : (
        inputs.map((i) => {
          const id = `dispatch-input-${i.name}`;
          const label = `${i.description || i.name}${i.required ? ' *' : ''}`;
          const set = (v: string) => setValues((s) => ({ ...s, [i.name]: v }));
          if (i.type === 'boolean') {
            return (
              <label key={i.name} className={styles.checkboxRow}>
                <input type="checkbox" checked={valueOf(i) === 'true'} onChange={(e) => set(e.target.checked ? 'true' : 'false')} />
                <span>{i.description || i.name}</span>
              </label>
            );
          }
          if (i.type === 'choice' || i.type === 'environment') {
            const opts = i.type === 'choice' ? i.options : (envs.data?.environments.map((e) => e.name) ?? []);
            const v = valueOf(i);
            return (
              <Field key={i.name} label={label} htmlFor={id}>
                <Select id={id} value={v} onChange={(e) => set(e.target.value)}>
                  {!opts.includes(v) && <option value={v}>{v || '—'}</option>}
                  {opts.map((o) => (
                    <option key={o} value={o}>
                      {o}
                    </option>
                  ))}
                </Select>
              </Field>
            );
          }
          return (
            <Field key={i.name} label={label} htmlFor={id}>
              <Input id={id} type={i.type === 'number' ? 'number' : 'text'} value={valueOf(i)} required={i.required} onChange={(e) => set(e.target.value)} />
            </Field>
          );
        })
      )}
      {error && <div className={styles.formError}>{error}</div>}
      <Button type="submit" variant="primary" block loading={busy} disabled={!form.data?.dispatchable || missing.length > 0}>
        Run workflow
      </Button>
    </form>
  );
}
