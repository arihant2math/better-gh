/**
 * Pieces shared by the organization runner groups page and the site admin
 * runners page (package P29): label chips, runner status, copyable
 * commands, the runner group create/edit dialog and the "add runner" row.
 */
import { useState, type ReactNode } from 'react';
import type { Runner, RunnerLabel } from '@/api/actions';
import { parseWorkflows, runnerArch, type GroupVisibility, type RunnerGroup } from '@/api/runners';
import { CopyButton, RadioCards, Switch, errorMessage } from '@/components/admin/kit';
import { Button, cx } from '@/ui/Button';
import { Dialog } from '@/ui/Dialog';
import { Skeleton } from '@/ui/EmptyState';
import { AlertIcon, LockIcon, PlusIcon } from '@/ui/icons';
import { Field, Input, Select, Textarea } from '@/ui/Input';
import s from './runnerKit.module.css';

export { s as runnerStyles };

export function LabelChips({ labels }: { labels: readonly RunnerLabel[] }) {
  return (
    <span className={s.chips}>
      {labels.map((l) =>
        l.type === 'read-only' ? (
          <span key={l.name} className={cx(s.chip, s.chipSystem)} title="Default label (read-only)">
            <LockIcon size={10} />
            {l.name}
          </span>
        ) : (
          <span key={l.name} className={s.chip} title="Custom label">
            {l.name}
          </span>
        ),
      )}
    </span>
  );
}

/** Plain string labels (requested by a job). */
export function NameChips({ names }: { names: readonly string[] }) {
  return (
    <span className={s.chips}>
      {names.map((n) => (
        <span key={n} className={s.chip}>
          {n}
        </span>
      ))}
    </span>
  );
}

/** Online / offline dot + label, and busy / idle when online. */
export function RunnerStatus({ runner }: { runner: Pick<Runner, 'status' | 'busy'> }) {
  const online = runner.status === 'online';
  return (
    <span className={s.status}>
      <span className={cx(s.dot, online && (runner.busy ? s.dotBusy : s.dotOnline))} aria-hidden />
      {online ? (runner.busy ? 'Active' : 'Idle') : 'Offline'}
    </span>
  );
}

/** `Linux · X64` from the runner's OS and read-only labels. */
export function osArch(r: Pick<Runner, 'os' | 'labels'> & { arch?: string }): string {
  const arch = r.arch || runnerArch(r);
  return arch ? `${r.os} · ${arch}` : r.os;
}

export function CommandBlock({ label, text, hint }: { label: string; text: string; hint?: ReactNode }) {
  return (
    <div>
      <div className={s.fieldLabel}>{label}</div>
      <div className={s.command}>
        <pre className={s.code}>{text}</pre>
        <CopyButton text={text} label={`Copy: ${label}`} />
      </div>
      {hint && <p className={s.hint}>{hint}</p>}
    </div>
  );
}

export function visibilityText(g: Pick<RunnerGroup, 'visibility'>, kind: 'org' | 'site'): string {
  if (kind === 'site') return g.visibility === 'selected' ? 'Selected organizations' : 'All organizations';
  return g.visibility === 'selected' ? 'Selected repositories' : g.visibility === 'private' ? 'Private repositories' : 'All repositories';
}

export interface GroupValues {
  name: string;
  visibility: GroupVisibility;
  selected: number[];
  allows_public_repositories: boolean;
  restricted_to_workflows: boolean;
  selected_workflows: string[];
}

export interface Target {
  id: number;
  label: string;
  private?: boolean;
}

/** `owner/repo/.github/workflows/file.yml@ref` */
const WORKFLOW_RE = /^[^/\s]+\/[^/\s]+\/\.github\/workflows\/[^@\s]+@\S+$/;

/**
 * Create / edit a runner group. `kind` picks the access targets:
 * repositories of the organization, or organizations of the instance.
 * `initialSelected` is null while the current selection is loading.
 */
export function GroupDialog({
  open,
  onClose,
  kind,
  group,
  targets,
  targetsError,
  initialSelected,
  onSubmit,
}: {
  open: boolean;
  onClose: () => void;
  kind: 'org' | 'site';
  group: RunnerGroup | null;
  targets: Target[] | undefined;
  targetsError?: unknown;
  initialSelected: number[] | null;
  onSubmit: (v: GroupValues) => Promise<void>;
}) {
  return (
    <Dialog open={open} onClose={onClose} title={group ? `Edit ${group.name}` : 'New runner group'}>
      {open && (
        <GroupForm
          key={group?.id ?? 'new'}
          kind={kind}
          group={group}
          targets={targets}
          targetsError={targetsError}
          initialSelected={initialSelected}
          onSubmit={onSubmit}
          onClose={onClose}
        />
      )}
    </Dialog>
  );
}

function GroupForm({
  kind,
  group,
  targets,
  targetsError,
  initialSelected,
  onSubmit,
  onClose,
}: {
  kind: 'org' | 'site';
  group: RunnerGroup | null;
  targets: Target[] | undefined;
  targetsError?: unknown;
  initialSelected: number[] | null;
  onSubmit: (v: GroupValues) => Promise<void>;
  onClose: () => void;
}) {
  const [name, setName] = useState(group?.name ?? '');
  const [visibility, setVisibility] = useState<GroupVisibility>(group?.visibility ?? 'all');
  const [picked, setPicked] = useState<Set<number> | null>(null);
  const selected = picked ?? (initialSelected ? new Set(initialSelected) : group && group.visibility === 'selected' ? null : new Set<number>());
  const [allowsPublic, setAllowsPublic] = useState(group?.allows_public_repositories ?? false);
  const [restricted, setRestricted] = useState(group?.restricted_to_workflows ?? false);
  const [workflows, setWorkflows] = useState((group?.selected_workflows ?? []).join('\n'));
  const [filter, setFilter] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const noun = kind === 'org' ? 'repositories' : 'organizations';

  const options: { value: GroupVisibility; label: string; description: string }[] =
    kind === 'org'
      ? [
          { value: 'all', label: 'All repositories', description: 'Every repository in the organization can use these runners.' },
          { value: 'private', label: 'Private repositories', description: 'Only private and internal repositories.' },
          { value: 'selected', label: 'Selected repositories', description: 'Only the repositories you pick below.' },
        ]
      : [
          { value: 'all', label: 'All organizations', description: 'Every organization on this instance can use these runners.' },
          { value: 'selected', label: 'Selected organizations', description: 'Only the organizations you pick below.' },
        ];

  const submit = async () => {
    const trimmed = name.trim();
    if (!trimmed) return setError('Name is required.');
    const wf = parseWorkflows(workflows);
    if (restricted) {
      const bad = wf.find((w) => !WORKFLOW_RE.test(w));
      if (bad) return setError(`“${bad}” is not a workflow reference (owner/repo/.github/workflows/file.yml@ref).`);
      if (!wf.length) return setError('List at least one workflow, or turn off the workflow restriction.');
    }
    if (visibility === 'selected' && !selected) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit({
        name: trimmed,
        visibility,
        selected: visibility === 'selected' ? [...selected!] : [],
        allows_public_repositories: allowsPublic,
        restricted_to_workflows: restricted,
        selected_workflows: restricted ? wf : [],
      });
      onClose();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const q = filter.trim().toLowerCase();
  const shown = (targets ?? []).filter((t) => !q || t.label.toLowerCase().includes(q));
  return (
    <form
      className={s.stack}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <Field label="Name" htmlFor="group-name" hint={group?.default ? 'The default group can’t be renamed.' : undefined}>
        <Input id="group-name" value={name} onChange={(e) => setName(e.target.value)} disabled={group?.default} autoFocus={!group} data-autofocus={!group || undefined} maxLength={100} />
      </Field>
      <div>
        <div className={s.fieldLabel}>{kind === 'org' ? 'Repository access' : 'Organization access'}</div>
        <RadioCards name="group-visibility" label={kind === 'org' ? 'Repository access' : 'Organization access'} value={visibility} onChange={setVisibility} options={options} />
      </div>
      {visibility === 'selected' && (
        <fieldset className={s.picker}>
          <legend className={s.pickerLegend}>
            Selected {noun} {selected && <span className={s.hint}>({selected.size})</span>}
          </legend>
          <Input size="sm" placeholder={`Filter ${noun}`} aria-label={`Filter ${noun}`} value={filter} onChange={(e) => setFilter(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && e.preventDefault()} />
          <div className={s.pickerList}>
            {targetsError ? (
              <div className={s.hint}>
                Couldn’t load {noun}: {errorMessage(targetsError)}
              </div>
            ) : !targets || !selected ? (
              <Skeleton height={60} />
            ) : shown.length === 0 ? (
              <div className={s.hint}>No {noun}</div>
            ) : (
              shown.map((t) => (
                <label key={t.id} className={s.pickerItem}>
                  <input
                    type="checkbox"
                    checked={selected.has(t.id)}
                    onChange={(e) => {
                      const n = new Set(selected);
                      if (e.target.checked) n.add(t.id);
                      else n.delete(t.id);
                      setPicked(n);
                    }}
                  />
                  {t.private && <LockIcon size={12} />}
                  {t.label}
                </label>
              ))
            )}
          </div>
        </fieldset>
      )}
      <Switch checked={allowsPublic} onChange={setAllowsPublic} label="Allow public repositories" description="Runners in this group can run jobs from public repositories. Public forks can run untrusted code on them." />
      <Switch checked={restricted} onChange={setRestricted} label="Restrict to selected workflows" description="Only the workflows listed below may use runners in this group." />
      {restricted && (
        <Field label="Allowed workflows" htmlFor="group-workflows" hint="One per line: owner/repo/.github/workflows/file.yml@refs/heads/main (the ref may be a branch, tag or SHA).">
          <Textarea
            id="group-workflows"
            className={s.workflows}
            value={workflows}
            placeholder="acme/api/.github/workflows/release.yml@refs/heads/main"
            onChange={(e) => setWorkflows(e.target.value)}
            spellCheck={false}
          />
        </Field>
      )}
      {error && (
        <div className={s.formError} role="alert">
          <AlertIcon size={14} /> {error}
        </div>
      )}
      <div className={s.actions}>
        <Button type="button" onClick={onClose}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" loading={busy} disabled={visibility === 'selected' && !selected}>
          {group ? 'Save changes' : 'Create group'}
        </Button>
      </div>
    </form>
  );
}

/** "Add runner" row: pick a runner of the same scope that isn't in this group. */
export function AddRunnerRow({
  candidates,
  groupName,
  onAdd,
}: {
  candidates: { id: number; name: string; groupName?: string | null }[];
  groupName: string;
  onAdd: (id: number) => Promise<void>;
}) {
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  if (!candidates.length) return <div className={s.empty}>Every runner of this scope is already in {groupName}.</div>;
  return (
    <div className={s.addRow}>
      <Select aria-label={`Runner to move into ${groupName}`} value={value} onChange={(e) => setValue(e.target.value)} style={{ height: 'var(--control-h-sm)' }}>
        <option value="">Move a runner into this group…</option>
        {candidates.map((r) => (
          <option key={r.id} value={r.id}>
            {r.name}
            {r.groupName ? ` (from ${r.groupName})` : ''}
          </option>
        ))}
      </Select>
      <Button
        size="sm"
        leadingIcon={PlusIcon}
        disabled={!value}
        loading={busy}
        onClick={() => {
          setBusy(true);
          onAdd(Number(value)).then(
            () => {
              setValue('');
              setBusy(false);
            },
            () => setBusy(false),
          );
        }}
      >
        Add runner
      </Button>
    </div>
  );
}
