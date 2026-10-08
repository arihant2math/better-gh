import { useEffect, useId, useState } from 'react';
import { createDeployKey, deleteDeployKey, listDeployKeys, type DeployKey } from '@/api/repoSettings';
import { invalidate } from '@/api/cache';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, Section, apiFieldErrors } from '@/components/settings/kit';
import { Link, navigate } from '@/router';
import type { Repo } from '@/sync/models';
import { Button } from '@/ui/Button';
import { KeyIcon, PlusIcon, TrashIcon } from '@/ui/icons';
import { Field, Input, Textarea } from '@/ui/Input';
import { RelativeTime } from '@/ui/RelativeTime';
import { toast } from '@/ui/Toast';
import styles from '../RepoSettings.module.css';
import { ListSkeleton, LoadError, repoKey, useLocalResource, type SectionProps } from '../shared';
import { sshKeyError } from '../validation';

export default function DeployKeysSettings({ repo, rest, base }: SectionProps) {
  if (rest[0] === 'new') return <NewDeployKey repo={repo} base={base} />;
  return <DeployKeyList repo={repo} base={base} />;
}

/** SHA256 fingerprint of the key blob (same as `ssh-keygen -lf`). */
async function fingerprint(key: string): Promise<string | null> {
  const b64 = key.trim().split(/\s+/)[1];
  if (!b64 || !crypto.subtle) return null;
  try {
    const bin = atob(b64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
    return `SHA256:${btoa(String.fromCharCode(...digest)).replace(/=+$/, '')}`;
  } catch {
    return null;
  }
}

function Fingerprint({ k }: { k: DeployKey }) {
  const [fp, setFp] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    void fingerprint(k.key).then((v) => !cancelled && setFp(v));
    return () => {
      cancelled = true;
    };
  }, [k.key]);
  return <span className={styles.mono}>{fp ?? k.key.slice(0, 32) + '…'}</span>;
}

function DeployKeyList({ repo, base }: { repo: Repo; base: string }) {
  const keys = useLocalResource(repoKey(repo, 'keys'), () => listDeployKeys(repo.owner, repo.name));
  const [deleting, setDeleting] = useState<DeployKey | null>(null);
  return (
    <>
      <PageHeader
        title="Deploy keys"
        description="Deploy keys grant access to this single repository over SSH. Read-only keys can clone; write keys can also push."
        actions={
          <Button variant="primary" size="sm" leadingIcon={PlusIcon} disabled={repo.archived} onClick={() => navigate(`${base}/keys/new`)}>
            Add deploy key
          </Button>
        }
      />
      {keys.error ? <LoadError error={keys.error} /> : null}
      {!keys.data ? (
        keys.error ? null : <ListSkeleton rows={2} />
      ) : (
        <ItemList aria-label="Deploy keys" empty="There are no deploy keys for this repository.">
          {keys.data.map((k) => (
            <ItemRow
              key={k.id}
              icon={KeyIcon}
              title={
                <span className={styles.row}>
                  {k.title || 'Untitled key'}
                  <Pill tone={k.read_only ? 'neutral' : 'warning'}>{k.read_only ? 'Read-only' : 'Read/write'}</Pill>
                </span>
              }
              meta={
                <>
                  <Fingerprint k={k} />
                  <br />
                  Added <RelativeTime date={k.created_at} />
                  {k.added_by ? ` by ${k.added_by}` : ''} · {k.last_used ? <>Last used <RelativeTime date={k.last_used} /></> : 'Never used'}
                </>
              }
              actions={
                <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={() => setDeleting(k)} aria-label={`Delete deploy key ${k.title}`}>
                  Delete
                </Button>
              }
            />
          ))}
        </ItemList>
      )}
      <ConfirmDialog
        open={!!deleting}
        onClose={() => setDeleting(null)}
        title="Delete deploy key?"
        confirmLabel="I understand, delete this key"
        onConfirm={async () => {
          if (!deleting) return;
          await deleteDeployKey(repo.owner, repo.name, deleting.id);
          keys.update((l) => l.filter((k) => k.id !== deleting.id));
          toast({ kind: 'success', title: `Deploy key “${deleting.title}” deleted` });
        }}
      >
        <p className={styles.muted}>
          Any machine using <strong>{deleting?.title}</strong> will no longer be able to access {repo.owner}/{repo.name}.
        </p>
      </ConfirmDialog>
    </>
  );
}

function NewDeployKey({ repo, base }: { repo: Repo; base: string }) {
  const ids = { title: useId(), key: useId() };
  const [title, setTitle] = useState('');
  const [key, setKey] = useState('');
  const [write, setWrite] = useState(false);
  const [touched, setTouched] = useState<{ title?: boolean; key?: boolean }>({});
  const [server, setServer] = useState<{ message: string | null; fields: Record<string, string | undefined> }>({ message: null, fields: {} });
  const [busy, setBusy] = useState(false);
  const titleErr = title.trim() ? (title.length > 100 ? 'Title is too long (maximum is 100 characters).' : null) : 'Title is required.';
  const keyErr = sshKeyError(key);

  const submit = async () => {
    setTouched({ title: true, key: true });
    if (titleErr || keyErr || busy) return;
    setBusy(true);
    setServer({ message: null, fields: {} });
    try {
      await createDeployKey(repo.owner, repo.name, { title: title.trim(), key: key.trim(), read_only: !write });
      invalidate(repoKey(repo, 'keys'));
      toast({ kind: 'success', title: `Deploy key “${title.trim()}” added` });
      navigate(`${base}/keys`);
    } catch (e) {
      setServer(apiFieldErrors(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <PageHeader title="Add deploy key" description={<Link to={`${base}/keys`}>← Deploy keys</Link>} />
      <Section>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <FormStack>
            <Field label="Title" htmlFor={ids.title} error={(touched.title && titleErr) || server.fields.title || null}>
              <Input
                id={ids.title}
                value={title}
                autoFocus
                invalid={!!(touched.title && titleErr) || !!server.fields.title}
                placeholder="e.g. CI deploy server"
                onChange={(e) => setTitle(e.target.value)}
                onBlur={() => setTouched((t) => ({ ...t, title: true }))}
              />
            </Field>
            <Field
              label="Key"
              htmlFor={ids.key}
              error={(touched.key && keyErr) || server.fields.key || null}
              hint="Begins with 'ssh-ed25519', 'ssh-rsa', 'ecdsa-sha2-nistp256', 'ecdsa-sha2-nistp384', 'ecdsa-sha2-nistp521', 'sk-ssh-ed25519@openssh.com' or 'sk-ecdsa-sha2-nistp256@openssh.com'."
            >
              <Textarea
                id={ids.key}
                value={key}
                rows={6}
                spellCheck={false}
                className={styles.mono}
                aria-invalid={!!(touched.key && keyErr) || !!server.fields.key || undefined}
                onChange={(e) => {
                  setKey(e.target.value);
                  if (server.fields.key) setServer((s) => ({ ...s, fields: { ...s.fields, key: undefined } }));
                }}
                onBlur={() => setTouched((t) => ({ ...t, key: true }))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    void submit();
                  }
                }}
              />
            </Field>
            <Checkbox
              label="Allow write access"
              description="Can this key be used to push to this repository? Deploy keys always have pull access."
              checked={write}
              onChange={setWrite}
            />
            {server.message && !server.fields.key && !server.fields.title && <Banner tone="danger">{server.message}</Banner>}
            <ButtonRow>
              <Button type="submit" variant="primary" loading={busy}>
                Add key
              </Button>
              <Button onClick={() => navigate(`${base}/keys`)}>Cancel</Button>
            </ButtonRow>
          </FormStack>
        </form>
      </Section>
    </>
  );
}
