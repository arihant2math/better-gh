import { useEffect, useRef, useState } from 'react';
import {
  addRunnerLabels,
  createRegistrationToken,
  deleteRunner,
  listRunners,
  removeRunnerLabel,
  type RegistrationToken,
  type Runner,
  type RunnerLabel,
  type SettingsScope,
} from '@/api/actions';
import { listOrgGroups, runnerArch } from '@/api/runners';
import { Link } from '@/router';
import { Tag } from '@/ui/Badge';
import { Button, IconButton, cx } from '@/ui/Button';
import { Dialog } from '@/ui/Dialog';
import { EmptyState } from '@/ui/EmptyState';
import { CheckIcon, CopyIcon, LockIcon, PlusIcon, ServerIcon, TrashIcon, XIcon } from '@/ui/icons';
import { Input } from '@/ui/Input';
import { RelativeTime } from '@/ui/RelativeTime';
import { Spinner } from '@/ui/Spinner';
import { toast } from '@/ui/Toast';
import { Tooltip } from '@/ui/Tooltip';
import { ConfirmDialog, ErrorState, ListSkeleton, Section, errorMessage, reload, scopeKey, toastError, useHidden, useRes, type Res } from './shared';
import styles from './Settings.module.css';

type RunnerScope = Exclude<SettingsScope, { kind: 'env' }>;

const runnersRes = (scope: RunnerScope): Res<Runner[]> => ({
  key: `${scopeKey(scope)}:runners`,
  load: () => listRunners(scope).then((r) => r.runners),
});

/** Labels: letters, digits and `-_.` (GitHub rejects others), max 100 chars. */
const LABEL_RE = /^[A-Za-z0-9._-]{1,100}$/;

export function Runners({ scope }: { scope: RunnerScope }) {
  const res = runnersRes(scope);
  const { data, error } = useRes(res);
  const [hidden, hide] = useHidden();
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<Runner | null>(null);
  const runners = (data ?? []).filter((r) => !hidden.has(String(r.id)));
  const where = scope.kind === 'org' ? 'organization' : 'repository';
  // Organization runners: show each runner's group (one cheap request).
  const groups = useRes(scope.kind === 'org' ? { key: `${scopeKey(scope)}:runner-groups`, load: () => listOrgGroups(scope.org) } : null);
  const groupName = (id: number | null | undefined) => (id == null ? undefined : groups.data?.find((g) => g.id === id)?.name);
  const groupsLink = scope.kind === 'org' ? `/organizations/${encodeURIComponent(scope.org)}/settings/actions/runner-groups` : null;

  // Runner status changes on its own: refresh while the page is open.
  useEffect(() => {
    const t = setInterval(() => {
      if (document.visibilityState === 'visible') void reload(runnersRes(scope));
    }, 15_000);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed by scope identity
  }, [res.key]);

  const newButton = (
    <Button size="sm" variant="primary" leadingIcon={PlusIcon} onClick={() => setAdding(true)}>
      New self-hosted runner
    </Button>
  );

  return (
    <Section
      title="Runners"
      description={
        <>
          Self-hosted runners registered to this {where}. Jobs run on a runner whose labels include every label in the job's runs-on.
          {groupsLink && (
            <>
              {' '}
              <Link to={groupsLink}>Manage runner groups</Link> to choose which repositories can use them.
            </>
          )}
        </>
      }
      action={!error ? newButton : undefined}
    >
      {error ? (
        <ErrorState error={error} what="runners" onRetry={() => void reload(res)} />
      ) : !data ? (
        <ListSkeleton rows={2} />
      ) : runners.length === 0 ? (
        <EmptyState icon={ServerIcon} title={`No self-hosted runners in this ${where}`} action={newButton}>
          Register a machine with bgh-runner to run jobs on your own hardware. Jobs can still use the server's built-in runner if it is enabled.
        </EmptyState>
      ) : (
        <div className={styles.list} role="list">
          {runners.map((r) => (
            <RunnerRow key={r.id} scope={scope} runner={r} group={groupName(r.runner_group_id)} onChanged={() => void reload(res)} onRemove={() => setRemoving(r)} />
          ))}
        </div>
      )}
      <p className={styles.note}>
        The server's built-in runner (<code className={styles.inlineCode}>BGH_ACTIONS_BUILTIN_RUNNER</code>) is a site-wide runner available to every
        repository; it is configured on the server and not listed here.
      </p>
      <Dialog open={adding} onClose={() => setAdding(false)} title="New self-hosted runner">
        {adding && <RegisterRunner scope={scope} onClose={() => setAdding(false)} />}
      </Dialog>
      <ConfirmDialog
        open={!!removing}
        title="Remove runner"
        confirmLabel="Remove runner"
        onClose={() => setRemoving(null)}
        onConfirm={() => {
          const r = removing;
          if (!r) return;
          hide(
            String(r.id),
            async () => {
              await deleteRunner(scope, r.id);
              await reload(res);
              toast({ kind: 'success', title: `Removed runner ${r.name}` });
            },
            (e) => toastError(`Couldn't remove ${r.name}`, e),
          );
        }}
      >
        Remove <strong>{removing?.name}</strong>? It will stop receiving jobs{removing?.busy ? ' and its current job will be lost' : ''}. Run{' '}
        <code className={styles.inlineCode}>bgh-runner remove</code> on the machine to clean up its configuration.
      </ConfirmDialog>
    </Section>
  );
}

function RunnerRow({ scope, runner, group, onChanged, onRemove }: { scope: RunnerScope; runner: Runner; group?: string; onChanged: () => void; onRemove: () => void }) {
  const arch = runnerArch(runner);
  // Optimistic label overlay until the refreshed list arrives.
  const [labels, setLabels] = useState<RunnerLabel[] | null>(null);
  const [base, setBase] = useState(runner.labels);
  if (base !== runner.labels) {
    setBase(runner.labels);
    setLabels(null);
  }
  const shown = labels ?? runner.labels;
  const [input, setInput] = useState<string | null>(null);
  const online = runner.status === 'online';

  const add = (raw: string) => {
    const names = raw
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean)
      .filter((n) => !shown.some((l) => l.name.toLowerCase() === n.toLowerCase()));
    const bad = names.find((n) => !LABEL_RE.test(n));
    if (bad) {
      toast({ kind: 'error', title: `Invalid label “${bad}”`, description: 'Use letters, digits, “.”, “-” and “_”.' });
      return;
    }
    setInput(null);
    if (!names.length) return;
    const prev = shown;
    setLabels([...prev, ...names.map((name, i) => ({ id: -1 - i, name, type: 'custom' as const }))]);
    addRunnerLabels(scope, runner.id, names).then(
      (r) => {
        setLabels(r.labels);
        onChanged();
      },
      (e: unknown) => {
        setLabels(prev);
        toastError("Couldn't add label", e);
      },
    );
  };

  const remove = (name: string) => {
    const prev = shown;
    setLabels(prev.filter((l) => l.name !== name));
    removeRunnerLabel(scope, runner.id, name).then(
      (r) => {
        setLabels(r.labels);
        onChanged();
      },
      (e: unknown) => {
        setLabels(prev);
        toastError(`Couldn't remove label ${name}`, e);
      },
    );
  };

  return (
    <div className={cx(styles.row, styles.runnerRow)} role="listitem">
      <ServerIcon size={16} className={styles.rowIcon} />
      <div className={styles.runnerMain}>
        <div className={styles.runnerTitle}>
          <span className={styles.runnerName}>{runner.name}</span>
          <span className={styles.meta}>
            {runner.os}
            {arch && ` · ${arch}`}
            {group && ` · ${group} group`}
          </span>
          {runner.ephemeral && <Tag>Ephemeral</Tag>}
        </div>
        <div className={styles.labels}>
          {shown.map((l) =>
            l.type === 'read-only' ? (
              <Tooltip key={l.name} label="Default label (read-only)">
                <span className={cx(styles.label, styles.labelSystem)}>
                  <LockIcon size={10} />
                  {l.name}
                </span>
              </Tooltip>
            ) : (
              <span key={l.name} className={styles.label}>
                {l.name}
                <button type="button" className={styles.labelRemove} aria-label={`Remove label ${l.name}`} onClick={() => remove(l.name)}>
                  <XIcon size={10} />
                </button>
              </span>
            ),
          )}
          {input === null ? (
            <button type="button" className={styles.labelAdd} onClick={() => setInput('')} aria-label={`Add label to ${runner.name}`}>
              <PlusIcon size={12} /> Label
            </button>
          ) : (
            <Input
              size="sm"
              className={styles.labelInput}
              autoFocus
              value={input}
              placeholder="gpu, arm64"
              aria-label={`New labels for ${runner.name}`}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  add(input);
                } else if (e.key === 'Escape') {
                  e.preventDefault();
                  e.stopPropagation();
                  setInput(null);
                }
              }}
              onBlur={() => (input.trim() ? add(input) : setInput(null))}
            />
          )}
        </div>
      </div>
      <span className={styles.status}>
        <span className={cx(styles.dot, online ? styles.dotOnline : styles.dotOffline)} aria-hidden />
        {online ? 'Online' : 'Offline'}
      </span>
      {online && <span className={cx(styles.badge, runner.busy ? styles.badgeBusy : styles.badgeIdle)}>{runner.busy ? 'Active' : 'Idle'}</span>}
      <span className={styles.rowActions}>
        <IconButton icon={TrashIcon} size="sm" label={`Remove runner ${runner.name}`} onClick={onRemove} />
      </span>
    </div>
  );
}

function RegisterRunner({ scope, onClose }: { scope: RunnerScope; onClose: () => void }) {
  const [state, setState] = useState<{ token?: RegistrationToken; error?: unknown }>({});
  const started = useRef(false);
  const fetchToken = () => {
    setState({});
    createRegistrationToken(scope).then(
      (token) => setState({ token }),
      (error: unknown) => setState({ error }),
    );
  };
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    fetchToken();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per dialog open
  }, []);

  if (state.error) {
    return (
      <div className={styles.dialogForm}>
        <ErrorState error={state.error} what="registration tokens" onRetry={fetchToken} />
      </div>
    );
  }
  if (!state.token) {
    return (
      <div className={styles.centered}>
        <Spinner /> Creating a registration token…
      </div>
    );
  }
  const { token, expires_at } = state.token;
  const origin = typeof window !== 'undefined' ? window.location.origin : '';
  const commands = `bgh-runner register --url ${origin} --token ${token} --name my-runner --labels self-hosted,linux\nbgh-runner run`;
  return (
    <div className={styles.dialogForm}>
      <div>
        <div className={styles.fieldLabel}>Registration token</div>
        <div className={styles.copyRow}>
          <code className={styles.token}>{token}</code>
          <CopyButton text={token} label="Copy token" />
        </div>
        <div className={styles.hint}>
          Expires <RelativeTime date={expires_at} />. Single use per registration; it is not shown again.
        </div>
      </div>
      <div>
        <div className={styles.fieldLabel}>Register and start the runner</div>
        <div className={styles.copyRow}>
          <pre className={styles.code}>{commands}</pre>
          <CopyButton text={commands} label="Copy commands" />
        </div>
        <div className={styles.hint}>
          Run these on the machine that should execute jobs. <code className={styles.inlineCode}>--labels</code> adds custom labels;{' '}
          <code className={styles.inlineCode}>--ephemeral</code> runs a single job and unregisters. <code className={styles.inlineCode}>bgh-runner run</code>{' '}
          accepts <code className={styles.inlineCode}>--executor auto|docker|shell</code> and{' '}
          <code className={styles.inlineCode}>--max-jobs N</code>.
        </div>
      </div>
      <p className={styles.note}>
        The server's built-in runner (<code className={styles.inlineCode}>BGH_ACTIONS_BUILTIN_RUNNER</code>) is a site-wide runner shared by all
        repositories; you don't need to register it.
      </p>
      <div className={styles.dialogActions}>
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      </div>
    </div>
  );
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(t);
  }, [copied]);
  return (
    <IconButton
      icon={copied ? CheckIcon : CopyIcon}
      size="sm"
      variant="secondary"
      label={copied ? 'Copied' : label}
      onClick={() =>
        void navigator.clipboard.writeText(text).then(
          () => setCopied(true),
          (e: unknown) => toast({ kind: 'error', title: "Couldn't copy", description: errorMessage(e) }),
        )
      }
    />
  );
}
