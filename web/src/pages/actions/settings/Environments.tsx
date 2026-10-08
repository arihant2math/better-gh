import { useId, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { deleteEnvironment, listEnvironments, putEnvironment, type Environment, type SettingsScope } from '@/api/actions';
import { Link } from '@/router';
import { Button, IconButton } from '@/ui/Button';
import { Dialog } from '@/ui/Dialog';
import { EmptyState } from '@/ui/EmptyState';
import { ChevronDownIcon, ChevronRightIcon, PlusIcon, RocketIcon, TrashIcon } from '@/ui/icons';
import { Field, Input } from '@/ui/Input';
import { RelativeTime } from '@/ui/RelativeTime';
import { toast } from '@/ui/Toast';
import { ConfigList } from './ConfigItems';
import { ConfirmDialog, ErrorState, ListSkeleton, Section, reload, scopeKey, toastError, useFocusOnOpen, useHidden, useRes, type Res } from './shared';
import styles from './Settings.module.css';
import { validateEnvironmentName } from './validation';

export const environmentsRes = (owner: string, repo: string): Res<Environment[]> => ({
  key: `${scopeKey({ kind: 'repo', owner, repo })}:environments`,
  load: () => listEnvironments(owner, repo).then((r) => r.environments),
});

const envScope = (owner: string, repo: string, env: string): SettingsScope => ({ kind: 'env', owner, repo, env });

/** Environments page: list, create, delete; each expands to its secrets and variables. */
export function Environments({ owner, repo }: { owner: string; repo: string }) {
  const res = environmentsRes(owner, repo);
  const { data, error } = useRes(res);
  const [hidden, hide] = useHidden();
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<Environment | null>(null);
  const [open, setOpen] = useState<Set<string>>(() => new Set());
  const envs = (data ?? []).filter((e) => !hidden.has(e.name));

  const toggle = (name: string) =>
    setOpen((o) => {
      const n = new Set(o);
      if (n.has(name)) n.delete(name);
      else n.add(name);
      return n;
    });

  const newButton = (
    <Button size="sm" variant="primary" leadingIcon={PlusIcon} onClick={() => setCreating(true)} disabled={!data}>
      New environment
    </Button>
  );

  return (
    <Section
      title="Environments"
      description="Deployment targets with their own secrets and variables. Jobs that set environment: <name> receive them."
      action={!error && envs.length > 0 ? newButton : undefined}
    >
      {error ? (
        <ErrorState error={error} what="environments" onRetry={() => void reload(res)} />
      ) : !data ? (
        <ListSkeleton rows={2} />
      ) : envs.length === 0 ? (
        <EmptyState icon={RocketIcon} title="There are no environments for this repository" action={newButton}>
          Environments are also created automatically the first time a workflow job references one.
        </EmptyState>
      ) : (
        <div className={styles.list} role="list">
          {envs.map((env) => {
            const expanded = open.has(env.name);
            const panelId = `env-${env.id}`;
            return (
              <div key={env.name} role="listitem" className={styles.envItem}>
                <div className={styles.row}>
                  <button
                    type="button"
                    className={styles.envToggle}
                    aria-expanded={expanded}
                    aria-controls={panelId}
                    onClick={() => toggle(env.name)}
                  >
                    {expanded ? <ChevronDownIcon size={16} /> : <ChevronRightIcon size={16} />}
                    <RocketIcon size={16} className={styles.rowIcon} />
                    <span className={styles.envName}>{env.name}</span>
                  </button>
                  <span className={styles.spacer} />
                  {(env.protection_rules?.length ?? 0) > 0 && (
                    <span className={styles.meta}>
                      {env.protection_rules!.length} protection rule{env.protection_rules!.length === 1 ? '' : 's'}
                    </span>
                  )}
                  <span className={styles.meta}>
                    Updated <RelativeTime date={env.updated_at} />
                  </span>
                  <Link to={`/${owner}/${repo}/settings/environments/${encodeURIComponent(env.name)}/edit`} className={styles.sectionLink}>
                    Configure
                  </Link>
                  <span className={styles.rowActions}>
                    <IconButton icon={TrashIcon} size="sm" label={`Delete environment ${env.name}`} onClick={() => setDeleting(env)} />
                  </span>
                </div>
                {expanded && (
                  <div id={panelId} className={styles.envPanel}>
                    <ConfigList scope={envScope(owner, repo, env.name)} kind="secrets" title="Environment secrets" compact />
                    <ConfigList scope={envScope(owner, repo, env.name)} kind="variables" title="Environment variables" compact />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
      <Dialog open={creating} onClose={() => setCreating(false)} title="New environment">
        {creating && (
          <NewEnvironment
            existing={(data ?? []).map((e) => e.name)}
            onClose={() => setCreating(false)}
            onCreate={async (name) => {
              await putEnvironment(owner, repo, name);
              await reload(res);
              setOpen((o) => new Set(o).add(name));
              toast({ kind: 'success', title: `Created environment ${name}` });
            }}
          />
        )}
      </Dialog>
      <ConfirmDialog
        open={!!deleting}
        title="Delete environment"
        confirmLabel="I understand, delete this environment"
        onClose={() => setDeleting(null)}
        onConfirm={() => {
          const env = deleting;
          if (!env) return;
          hide(
            env.name,
            async () => {
              await deleteEnvironment(owner, repo, env.name);
              await reload(res);
              toast({ kind: 'success', title: `Deleted environment ${env.name}` });
            },
            (e) => toastError(`Couldn't delete ${env.name}`, e),
          );
        }}
      >
        Deleting <strong>{deleting?.name}</strong> also deletes all of its secrets and variables. This can't be undone.
      </ConfirmDialog>
    </Section>
  );
}

function NewEnvironment({ existing, onClose, onCreate }: { existing: string[]; onClose: () => void; onCreate: (name: string) => Promise<void> }) {
  const id = useId();
  const [name, setName] = useState('');
  const [err, setErr] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const ref = useRef<HTMLInputElement>(null);
  useFocusOnOpen(ref);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const v = validateEnvironmentName(name, existing);
    setErr(v);
    if (v || saving) return;
    setSaving(true);
    try {
      await onCreate(name.trim());
      onClose();
    } catch (x) {
      toastError("Couldn't create environment", x);
      setSaving(false);
    }
  };
  return (
    <form className={styles.dialogForm} onSubmit={(e) => void submit(e)} noValidate>
      <Field label="Name" htmlFor={`${id}-name`} error={err} hint="For example production, staging or github-pages.">
        <Input
          ref={ref}
          id={`${id}-name`}
          value={name}
          autoComplete="off"
          invalid={!!err}
          onChange={(e) => {
            setName(e.target.value);
            if (err) setErr(null);
          }}
        />
      </Field>
      <div className={styles.dialogActions}>
        <Button onClick={onClose}>Cancel</Button>
        <Button type="submit" variant="primary" loading={saving}>
          Configure environment
        </Button>
      </div>
    </form>
  );
}

/**
 * Environment secrets / variables on the repository secrets / variables page,
 * grouped per environment (each with its own add / update / delete).
 */
export function EnvironmentGroups({ owner, repo, kind, envLink }: { owner: string; repo: string; kind: 'secrets' | 'variables'; envLink: string }) {
  const res = environmentsRes(owner, repo);
  const { data, error } = useRes(res);
  let body: ReactNode;
  if (error) body = <ErrorState error={error} what="environments" onRetry={() => void reload(res)} />;
  else if (!data) body = <ListSkeleton rows={1} />;
  else if (data.length === 0)
    body = (
      <div className={styles.emptyInline}>
        This repository has no environments. <Link to={envLink}>Manage environments</Link>
      </div>
    );
  else
    body = (
      <div className={styles.envGroups}>
        {data.map((env) => (
          <ConfigList key={env.name} scope={envScope(owner, repo, env.name)} kind={kind} title={env.name} compact />
        ))}
      </div>
    );
  return (
    <Section
      title={`Environment ${kind}`}
      description={`Available only to jobs that reference the environment. They override repository and organization ${kind} with the same name.`}
      action={
        data && data.length > 0 ? (
          <Link to={envLink} className={styles.sectionLink}>
            Manage environments
          </Link>
        ) : undefined
      }
    >
      {body}
    </Section>
  );
}
