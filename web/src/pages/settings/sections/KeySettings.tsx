import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import { createGpgKey, createSshKey, deleteGpgKey, deleteSshKey, listGpgKeys, listSshKeys, type GpgKey, type SshKey } from '../../../api/developerSettings';
import { apiFieldErrors, ButtonRow, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, Section } from '../../../components/settings/kit';
import { Button } from '../../../ui/Button';
import { KeyIcon, PlusIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input, Textarea } from '../../../ui/Input';
import { ListSkeleton, lastUsedText } from '../developer/common';
import styles from '../developer/developer.module.css';
import { formatDate } from '../developer/logic';
import { checkArmoredGpg, fingerprintOf, keyTypeLabel, parseSshKey } from '../developer/sshKey';
import { useList } from '../developer/useList';

/** `/settings/keys`: SSH authentication keys and GPG signing keys. */
export default function KeySettings() {
  return (
    <>
      <PageHeader title="SSH and GPG keys" description="Keys let you push over SSH and show commits you sign as verified." />
      <SshKeys />
      <GpgKeys />
    </>
  );
}

// ------------------------------------------------------------------ SSH

function SshKeys() {
  const list = useList<SshKey>('dev:ssh-keys', listSshKeys);
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<SshKey | null>(null);
  const fps = useFingerprints(list.items);
  return (
    <Section
      title="SSH keys"
      description="This is a list of SSH keys associated with your account. Remove any keys that you do not recognize."
      actions={
        !adding && (
          <Button variant="primary" size="sm" leadingIcon={PlusIcon} onClick={() => setAdding(true)}>
            New SSH key
          </Button>
        )
      }
    >
      {adding && (
        <AddSshKey
          onCancel={() => setAdding(false)}
          onAdded={(k) => {
            list.add(k);
            setAdding(false);
          }}
        />
      )}
      {list.items ? (
        <ItemList aria-label="SSH keys" empty="There are no SSH keys associated with your account.">
          {list.items.map((k) => (
            <ItemRow
              key={k.id}
              leading={
                <span className={styles.keyIcon} aria-hidden>
                  <KeyIcon size={24} />
                  <span className={styles.keyType}>{keyTypeLabel(k.key)}</span>
                </span>
              }
              title={k.title || <em>Untitled key</em>}
              actions={
                <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={() => setConfirm(k)} aria-label={`Delete SSH key ${k.title}`}>
                  Delete
                </Button>
              }
            >
              <div className={styles.metaLines}>
                <span className={styles.mono}>{fps[k.id] ?? ' '}</span>
                <span>
                  Added on {formatDate(k.created_at)} · {lastUsedText(k.last_used)}
                  {k.read_only && ' · Read-only'}
                </span>
              </div>
            </ItemRow>
          ))}
        </ItemList>
      ) : list.error ? (
        <ItemList empty="Could not load your SSH keys." />
      ) : (
        <ListSkeleton />
      )}
      <p className={styles.help}>
        Check out the guide to <code>ssh-keygen -t ed25519 -C &quot;you@example.com&quot;</code> to generate a key, then paste the contents of{' '}
        <code>~/.ssh/id_ed25519.pub</code>.
      </p>
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Delete SSH key"
        confirmLabel="I understand, delete this SSH key"
        onConfirm={() => {
          const k = confirm!;
          void list.remove(k.id, () => deleteSshKey(k.id), 'SSH key deleted');
        }}
      >
        <p>
          This action <strong>cannot be undone</strong>. Anything using <strong>{confirm?.title || 'this key'}</strong> will no longer be able to access your
          repositories over SSH.
        </p>
      </ConfirmDialog>
    </Section>
  );
}

function useFingerprints(keys: SshKey[] | undefined): Record<number, string> {
  const [fps, setFps] = useState<Record<number, string>>({});
  const sig = keys?.map((k) => k.id).join(',');
  useEffect(() => {
    if (!keys) return;
    let live = true;
    void Promise.all(keys.map(async (k) => [k.id, (await fingerprintOf(k.key)) ?? ''] as const)).then((pairs) => {
      if (live) setFps(Object.fromEntries(pairs));
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed by the id list
  }, [sig]);
  return fps;
}

function AddSshKey({ onCancel, onAdded }: { onCancel: () => void; onAdded: (k: SshKey) => void }) {
  const id = useId();
  const [title, setTitle] = useState('');
  const [titleTouched, setTitleTouched] = useState(false);
  const [key, setKey] = useState('');
  const [errors, setErrors] = useState<{
    title?: string;
    key?: string;
    form?: string;
  }>({});
  const [busy, setBusy] = useState(false);
  const keyRef = useRef<HTMLTextAreaElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  useEffect(() => titleRef.current?.focus(), []);

  const onKeyChange = (v: string) => {
    setKey(v);
    setErrors((e) => ({ ...e, key: undefined, form: undefined }));
    // Suggest the key comment as the title (user@host) unless the user typed one.
    const p = parseSshKey(v);
    if (p.ok && p.key.comment && (!titleTouched || !title)) setTitle(p.key.comment);
  };

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (busy) return;
    const p = parseSshKey(key);
    if (!p.ok) {
      setErrors({ key: p.error });
      keyRef.current?.focus();
      return;
    }
    setBusy(true);
    try {
      const created = await createSshKey({
        title: title.trim() || undefined,
        key: key.trim(),
      });
      onAdded(created);
    } catch (err) {
      const f = apiFieldErrors(err);
      setErrors({
        key: f.fields.key ? capitalize(f.fields.key) : undefined,
        title: f.fields.title,
        form: f.fields.key || f.fields.title ? undefined : f.message,
      });
      keyRef.current?.focus();
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className={styles.formCard}
      onSubmit={(e) => void submit(e)}
      onKeyDown={(e) => e.key === 'Escape' && onCancel()}
      aria-label="Add new SSH key"
      noValidate
    >
      <Field label="Title" htmlFor={`${id}-title`} error={errors.title} hint="A name to recognize this key by, e.g. the machine it lives on.">
        <Input
          id={`${id}-title`}
          ref={titleRef}
          value={title}
          maxLength={100}
          onChange={(e) => {
            setTitle(e.target.value);
            setTitleTouched(true);
          }}
          placeholder="e.g. Work laptop"
        />
      </Field>
      <Field
        label="Key"
        htmlFor={`${id}-key`}
        error={errors.key}
        hint="Begins with ssh-ed25519, ssh-rsa, ecdsa-sha2-nistp256, ecdsa-sha2-nistp384, ecdsa-sha2-nistp521, sk-ssh-ed25519@openssh.com or sk-ecdsa-sha2-nistp256@openssh.com"
      >
        <Textarea
          id={`${id}-key`}
          ref={keyRef}
          value={key}
          spellCheck={false}
          aria-invalid={!!errors.key || undefined}
          onChange={(e) => onKeyChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              void submit();
            }
          }}
          placeholder="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA… you@example.com"
        />
      </Field>
      {errors.form && (
        <div className={styles.help} role="alert">
          {errors.form}
        </div>
      )}
      <ButtonRow>
        <Button type="submit" variant="primary" loading={busy} disabled={!key.trim()}>
          Add SSH key
        </Button>
        <Button onClick={onCancel}>Cancel</Button>
      </ButtonRow>
    </form>
  );
}

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

// ------------------------------------------------------------------ GPG

function GpgKeys() {
  const list = useList<GpgKey>('dev:gpg-keys', listGpgKeys);
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<GpgKey | null>(null);
  return (
    <Section
      title="GPG keys"
      description="This is a list of GPG keys associated with your account. Remove any keys that you do not recognize."
      actions={
        !adding && (
          <Button variant="primary" size="sm" leadingIcon={PlusIcon} onClick={() => setAdding(true)}>
            New GPG key
          </Button>
        )
      }
    >
      {adding && (
        <AddGpgKey
          onCancel={() => setAdding(false)}
          onAdded={(k) => {
            list.add(k);
            setAdding(false);
          }}
        />
      )}
      {list.items ? (
        <ItemList aria-label="GPG keys" empty="There are no GPG keys associated with your account.">
          {list.items.map((k) => (
            <GpgRow key={k.id} k={k} onDelete={() => setConfirm(k)} />
          ))}
        </ItemList>
      ) : list.error ? (
        <ItemList empty="Could not load your GPG keys." />
      ) : (
        <ListSkeleton />
      )}
      <p className={styles.help}>
        Export a key with <code>gpg --armor --export &lt;KEY ID&gt;</code>. Commits signed with a key whose email is verified on your account show as Verified.
      </p>
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Delete GPG key"
        confirmLabel="I understand, delete this GPG key"
        onConfirm={() => {
          const k = confirm!;
          void list.remove(k.id, () => deleteGpgKey(k.id), 'GPG key deleted');
        }}
      >
        <p>
          Commits signed with <strong className={styles.mono}>{confirm?.key_id}</strong> will no longer show as verified. This action{' '}
          <strong>cannot be undone</strong>.
        </p>
      </ConfirmDialog>
    </Section>
  );
}

function GpgRow({ k, onDelete }: { k: GpgKey; onDelete: () => void }) {
  const expired = k.expires_at && Date.parse(k.expires_at) < Date.now();
  return (
    <ItemRow
      leading={
        <span className={styles.keyIcon} aria-hidden>
          <KeyIcon size={24} />
          <span className={styles.keyType}>GPG</span>
        </span>
      }
      title={
        <>
          {k.name && <span>{k.name}</span>}
          <span className={styles.mono}>Key ID: {k.key_id}</span>
          {expired && <Pill tone="danger">Expired</Pill>}
          {k.revoked && <Pill tone="danger">Revoked</Pill>}
        </>
      }
      actions={
        <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={onDelete} aria-label={`Delete GPG key ${k.key_id}`}>
          Delete
        </Button>
      }
    >
      <div className={styles.metaLines}>
        <span className={styles.emailList}>
          Email address{k.emails.length === 1 ? '' : 'es'}:{k.emails.length === 0 && <span>none</span>}
          {k.emails.map((e) => (
            <span key={e.email} className={styles.emailItem}>
              {e.email}
              {e.verified ? <Pill tone="success">Verified</Pill> : <Pill tone="warning">Unverified</Pill>}
            </span>
          ))}
        </span>
        {k.subkeys.length > 0 && (
          <span>
            Subkeys: <span className={styles.mono}>{k.subkeys.map((s) => s.key_id).join(', ')}</span>
          </span>
        )}
        <span>
          Added on {formatDate(k.created_at)} · {k.expires_at ? `${expired ? 'Expired' : 'Expires'} on ${formatDate(k.expires_at)}` : 'Does not expire'}
        </span>
      </div>
    </ItemRow>
  );
}

function AddGpgKey({ onCancel, onAdded }: { onCancel: () => void; onAdded: (k: GpgKey) => void }) {
  const id = useId();
  const [name, setName] = useState('');
  const [key, setKey] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  const keyRef = useRef<HTMLTextAreaElement>(null);
  useEffect(() => nameRef.current?.focus(), []);

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (busy) return;
    const err = checkArmoredGpg(key);
    if (err) {
      setError(err);
      keyRef.current?.focus();
      return;
    }
    setBusy(true);
    try {
      onAdded(
        await createGpgKey({
          name: name.trim() || undefined,
          armored_public_key: key.trim(),
        }),
      );
    } catch (x) {
      const f = apiFieldErrors(x);
      setError(f.fields.armored_public_key ?? f.fields.key_id ?? f.message);
      keyRef.current?.focus();
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className={styles.formCard}
      onSubmit={(e) => void submit(e)}
      onKeyDown={(e) => e.key === 'Escape' && onCancel()}
      aria-label="Add new GPG key"
      noValidate
    >
      <Field label="Title" htmlFor={`${id}-name`} hint="Optional.">
        <Input id={`${id}-name`} ref={nameRef} value={name} maxLength={100} onChange={(e) => setName(e.target.value)} placeholder="e.g. Signing key 2026" />
      </Field>
      <Field label="Key" htmlFor={`${id}-key`} error={error} hint='Begins with "-----BEGIN PGP PUBLIC KEY BLOCK-----"'>
        <Textarea
          id={`${id}-key`}
          ref={keyRef}
          value={key}
          spellCheck={false}
          rows={10}
          aria-invalid={!!error || undefined}
          onChange={(e) => {
            setKey(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void submit();
            }
          }}
          placeholder={'-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n…\n-----END PGP PUBLIC KEY BLOCK-----'}
        />
      </Field>
      <ButtonRow>
        <Button type="submit" variant="primary" loading={busy} disabled={!key.trim()} kbd="⌘↵">
          Add GPG key
        </Button>
        <Button onClick={onCancel}>Cancel</Button>
      </ButtonRow>
    </form>
  );
}
