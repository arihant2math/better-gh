import { useEffect, useId, useState, type FormEvent } from 'react';
import { invalidate } from '@/api/cache';
import { KEYS, useEditableResource } from '@/api/userSettings';
import {
  deleteCredential,
  listCredentials,
  registerCredential,
  renameCredential,
  webauthnError,
  webauthnSupported,
  type CredentialKind,
  type WebauthnCredential,
} from '@/api/webauthn';
import { isMockMode } from '@/boot';
import { Banner, FormStack, Pill, Section, errorMessage } from '@/components/settings/kit';
import { Button } from '@/ui/Button';
import { Dialog } from '@/ui/Dialog';
import { Box, Skeleton } from '@/ui/EmptyState';
import { KeyIcon, PencilIcon, ShieldLockIcon, TrashIcon } from '@/ui/icons';
import { Field, Input } from '@/ui/Input';
import { RelativeTime } from '@/ui/RelativeTime';
import { toast } from '@/ui/Toast';
import styles from './userSettings.module.css';

const COPY: Record<
  CredentialKind,
  {
    title: string;
    description: string;
    noun: string;
    add: string;
    placeholder: string;
  }
> = {
  passkey: {
    title: 'Passkeys',
    description: 'Passkeys let you sign in without a password, using your fingerprint, face, screen lock or a hardware security key.',
    noun: 'passkey',
    add: 'Add a passkey',
    placeholder: 'e.g. MacBook Touch ID',
  },
  security_key: {
    title: 'Security keys',
    description: 'Security keys are hardware or platform authenticators you can use as your second factor instead of an authenticator app code.',
    noun: 'security key',
    add: 'Register new security key',
    placeholder: 'e.g. YubiKey 5C',
  },
};

/** WebAuthn credentials of one kind: list, register, rename, delete. */
export function WebauthnSection({ kind, twoFactorEnabled, onChange }: { kind: CredentialKind; twoFactorEnabled: boolean; onChange?: () => void }) {
  const list = useEditableResource(KEYS.webauthn, listCredentials);
  const [adding, setAdding] = useState(false);
  const [renaming, setRenaming] = useState<WebauthnCredential | null>(null);
  const [removing, setRemoving] = useState<WebauthnCredential | null>(null);
  const copy = COPY[kind];
  const rows = (list.data ?? []).filter((c) => c.kind === kind);
  const supported = webauthnSupported() && !isMockMode();
  const needsTotp = kind === 'security_key' && !twoFactorEnabled;

  const changed = () => {
    invalidate(KEYS.twoFactor);
    onChange?.();
  };

  return (
    <Section
      title={
        <span className={styles.titleRow}>
          {copy.title} {rows.length > 0 && <Pill tone="success">{rows.length}</Pill>}
        </span>
      }
      description={copy.description}
    >
      {list.error && !list.data ? (
        <Banner tone="danger">{errorMessage(list.error)}</Banner>
      ) : !list.data ? (
        <Skeleton height={56} />
      ) : (
        <Box>
          {rows.map((c) => (
            <div key={c.id} className={styles.factorRow} data-testid={`webauthn-${kind}`}>
              {kind === 'passkey' ? <KeyIcon size={20} /> : <ShieldLockIcon size={20} />}
              <div className={styles.grow}>
                <div className={styles.strong}>{c.name}</div>
                <div className={styles.muted}>
                  Added <RelativeTime date={c.created_at} />
                  {c.last_used_at ? (
                    <>
                      {' '}
                      · last used <RelativeTime date={c.last_used_at} />
                    </>
                  ) : (
                    ' · never used'
                  )}
                </div>
              </div>
              <Button size="sm" leadingIcon={PencilIcon} onClick={() => setRenaming(c)}>
                Rename
              </Button>
              <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={() => setRemoving(c)}>
                Delete
              </Button>
            </div>
          ))}
          <div className={styles.factorRow}>
            <div className={styles.grow}>
              <div className={styles.muted}>
                {!supported
                  ? 'This browser does not support security keys and passkeys here.'
                  : needsTotp
                    ? 'Set up an authenticator app first; security keys are an additional second factor.'
                    : rows.length === 0
                      ? `No ${copy.noun}s registered yet.`
                      : `You have ${rows.length} ${copy.noun}${rows.length === 1 ? '' : 's'}.`}
              </div>
            </div>
            <Button size="sm" variant={rows.length === 0 ? 'primary' : 'secondary'} disabled={!supported || needsTotp} onClick={() => setAdding(true)}>
              {copy.add}
            </Button>
          </div>
        </Box>
      )}
      <NameDialog
        open={adding}
        title={copy.add}
        label={`Name your ${copy.noun}`}
        placeholder={copy.placeholder}
        confirm="Continue"
        hint="Your browser will ask you to use the authenticator next."
        onClose={() => setAdding(false)}
        onSubmit={async (name) => {
          try {
            const c = await registerCredential(kind, name);
            list.update((prev) => [...prev, c]);
            toast({
              kind: 'success',
              title: `${copy.noun[0]!.toUpperCase()}${copy.noun.slice(1)} registered`,
            });
            changed();
          } catch (e) {
            throw new Error(e instanceof DOMException ? webauthnError(e) : errorMessage(e), { cause: e });
          }
        }}
      />
      <NameDialog
        open={!!renaming}
        title={`Rename ${copy.noun}`}
        label="Name"
        initial={renaming?.name}
        confirm="Save"
        onClose={() => setRenaming(null)}
        onSubmit={async (name) => {
          const c = await renameCredential(renaming!.id, name);
          list.update((prev) => prev.map((x) => (x.id === c.id ? c : x)));
        }}
      />
      <Dialog
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={`Delete ${copy.noun}`}
        footer={
          <>
            <Button onClick={() => setRemoving(null)}>Cancel</Button>
            <Button
              variant="danger"
              onClick={() => {
                const c = removing!;
                setRemoving(null);
                void deleteCredential(c.id).then(
                  () => {
                    list.update((prev) => prev.filter((x) => x.id !== c.id));
                    changed();
                  },
                  (e: unknown) => toast({ kind: 'error', title: errorMessage(e) }),
                );
              }}
            >
              Delete
            </Button>
          </>
        }
      >
        <p className={styles.muted}>
          <strong>{removing?.name}</strong> will no longer be able to {kind === 'passkey' ? 'sign in to' : 'verify sign-ins to'} your account.
        </p>
      </Dialog>
    </Section>
  );
}

function NameDialog({
  open,
  title,
  label,
  placeholder,
  initial = '',
  hint,
  confirm,
  onClose,
  onSubmit,
}: {
  open: boolean;
  title: string;
  label: string;
  placeholder?: string;
  initial?: string;
  hint?: string;
  confirm: string;
  onClose: () => void;
  onSubmit: (name: string) => Promise<void>;
}) {
  const [name, setName] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const id = useId();
  useEffect(() => {
    if (open) {
      setName(initial);
      setError(null);
    }
  }, [open, initial]);
  const submit = async (ev?: FormEvent) => {
    ev?.preventDefault();
    if (busy) return;
    const n = name.trim();
    if (!n) return setError('Enter a name');
    if (n.length > 64) return setError('Use 64 characters or fewer');
    setBusy(true);
    setError(null);
    try {
      await onSubmit(n);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} onClick={() => void submit()}>
            {confirm}
          </Button>
        </>
      }
    >
      <form onSubmit={(e) => void submit(e)} noValidate>
        <FormStack>
          <Field label={label} htmlFor={id} error={error} hint={hint}>
            <Input id={id} autoFocus maxLength={64} placeholder={placeholder} value={name} invalid={!!error} onChange={(e) => setName(e.target.value)} />
          </Field>
        </FormStack>
      </form>
    </Dialog>
  );
}
