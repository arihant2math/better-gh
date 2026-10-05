import { useEffect, useId, useState } from 'react';
import { deleteAccount, loginError, renameErrorMessage, renameUser } from '../../../api/lifecycle';
import { applyViewerPatch } from '../../../api/userSettings';
import { session } from '../../../app/session';
import { Banner, errorMessage } from '../../../components/settings/kit';
import { Button } from '../../../ui/Button';
import { Dialog } from '../../../ui/Dialog';
import { AlertIcon, InfoIcon } from '../../../ui/icons';
import { Field, Input } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import styles from './userSettings.module.css';

/** `PATCH /user {login}`: old links and git remotes redirect, the old name is reserved for 90 days. */
export function RenameUserDialog({ open, onClose, login }: { open: boolean; onClose: () => void; login: string }) {
  const id = useId();
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setValue('');
      setError(null);
    }
  }, [open]);
  const next = value.trim();
  const localError = next ? loginError(next) : null;
  const same = next === login;
  const submit = async () => {
    if (!next || localError || same || busy) return;
    setBusy(true);
    setError(null);
    try {
      const me = await renameUser(next);
      applyViewerPatch({ login: me.login }).settled();
      toast({ kind: 'success', title: `Your username is now ${me.login}` });
      onClose();
    } catch (e) {
      setError(renameErrorMessage(e, next));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Change your username"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="danger" disabled={!next || !!localError || same} loading={busy} onClick={() => void submit()}>
            Change my username
          </Button>
        </>
      }
    >
      <form
        className={styles.dialogStack}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Banner tone="warning" icon={AlertIcon}>
          <strong>Unexpected bad things will happen if you don’t read this.</strong>
          <ul className={styles.warnList}>
            <li>
              Links to your profile and repositories, and git remotes using <span className={styles.mono}>{login}</span>, redirect to the new name.
            </li>
            <li>
              The name <span className={styles.mono}>{login}</span> is reserved for you for 90 days; after that anyone can claim it and the redirects stop.
            </li>
            <li>You can change your username at most 3 times in 24 hours.</li>
          </ul>
        </Banner>
        <Field label="New username" htmlFor={id} error={error ?? localError}>
          <Input
            id={id}
            data-autofocus
            value={value}
            placeholder={login}
            autoComplete="off"
            spellCheck={false}
            invalid={!!(error ?? localError)}
            onChange={(e) => {
              setValue(e.target.value);
              setError(null);
            }}
          />
        </Field>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

/** `DELETE /user {password}`: type the login and the password (or a 2FA code) to confirm. */
export function DeleteAccountDialog({ open, onClose, login }: { open: boolean; onClose: () => void; login: string }) {
  const loginId = useId();
  const pwId = useId();
  const [typed, setTyped] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setTyped('');
      setPassword('');
      setError(null);
    }
  }, [open]);
  const ok = typed.trim() === login && password !== '';
  const submit = async () => {
    if (!ok || busy) return;
    setBusy(true);
    setError(null);
    try {
      await deleteAccount(password);
      toast({ kind: 'success', title: 'Your account has been deleted' });
      onClose();
      // The server ended the session: drop local data and leave.
      await session.logout();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Are you sure you want to do this?"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="danger" disabled={!ok} loading={busy} onClick={() => void submit()}>
            Delete this account
          </Button>
        </>
      }
    >
      <form
        className={styles.dialogStack}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Banner tone="danger" icon={AlertIcon}>
          This is extremely important. Your repositories are deleted, and your issues, pull requests and comments are attributed to a ghost user. Organizations you are the
          only owner of must get another owner or be deleted first.
        </Banner>
        <Field label={`To confirm, type your username "${login}"`} htmlFor={loginId}>
          <Input id={loginId} data-autofocus value={typed} autoComplete="off" spellCheck={false} onChange={(e) => setTyped(e.target.value)} />
        </Field>
        <Field label="Confirm your password" htmlFor={pwId} hint="Accounts without a password: enter a two-factor authentication code instead.">
          <Input
            id={pwId}
            type="password"
            value={password}
            autoComplete="current-password"
            invalid={!!error}
            onChange={(e) => {
              setPassword(e.target.value);
              setError(null);
            }}
          />
        </Field>
        {error && (
          <Banner tone="danger" icon={InfoIcon}>
            {error}
          </Banner>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
