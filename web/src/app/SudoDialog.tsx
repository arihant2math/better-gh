import { useEffect, useId, useState, type FormEvent } from 'react';
import { getSudo, sudoWithCode, sudoWithPassword, sudoWithSecurityKey, webauthnError, webauthnSupported, type SudoStatus } from '../api/webauthn';
import { Button } from '../ui/Button';
import { Dialog } from '../ui/Dialog';
import { DeviceMobileIcon, KeyIcon, ShieldLockIcon } from '../ui/icons';
import { Field, Input } from '../ui/Input';
import styles from './SudoDialog.module.css';

type Method = 'password' | 'totp';

/** "Confirm access": re-authenticate with a password, a 2FA code or a security key. */
export default function SudoDialog({ onDone }: { onDone: (ok: boolean) => void }) {
  const [status, setStatus] = useState<SudoStatus | null>(null);
  const [method, setMethod] = useState<Method>('password');
  const [value, setValue] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const id = useId();

  useEffect(() => {
    let live = true;
    getSudo().then(
      (s) => {
        if (!live) return;
        if (s.active) return onDone(true);
        setStatus(s);
        if (!s.methods.password && s.methods.totp) setMethod('totp');
      },
      () =>
        live &&
        setStatus({
          active: false,
          expires_at: null,
          methods: { password: true, totp: false, webauthn: false },
        }),
    );
    return () => {
      live = false;
    };
  }, [onDone]);

  const run = async (f: () => Promise<SudoStatus>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      onDone(true);
    } catch (e) {
      setError(webauthnError(e));
      setValue('');
    } finally {
      setBusy(false);
    }
  };

  const submit = (ev?: FormEvent) => {
    ev?.preventDefault();
    if (busy) return;
    if (!value.trim()) return setError(method === 'password' ? 'Enter your password' : 'Enter a two-factor or recovery code');
    void run(() => (method === 'password' ? sudoWithPassword(value) : sudoWithCode(value.trim())));
  };

  const m = status?.methods;
  const canKey = !!m?.webauthn && webauthnSupported();
  return (
    <Dialog
      open
      onClose={() => onDone(false)}
      title="Confirm access"
      footer={
        <>
          <Button onClick={() => onDone(false)}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!status} onClick={() => submit()}>
            Confirm
          </Button>
        </>
      }
    >
      <form onSubmit={submit} noValidate className={styles.form} data-testid="sudo-dialog">
        <p className={styles.muted}>
          <ShieldLockIcon size={14} /> You are entering sudo mode. You won't be asked again for the next two hours.
        </p>
        <Field label={method === 'password' ? 'Password' : 'Authentication code'} htmlFor={id} error={error}>
          <Input
            id={id}
            autoFocus
            type={method === 'password' ? 'password' : 'text'}
            autoComplete={method === 'password' ? 'current-password' : 'one-time-code'}
            inputMode={method === 'password' ? undefined : 'numeric'}
            value={value}
            invalid={!!error}
            disabled={!status}
            onChange={(e) => setValue(e.target.value)}
          />
        </Field>
        <div className={styles.alternatives}>
          {canKey && (
            <Button size="sm" leadingIcon={KeyIcon} disabled={busy} onClick={() => void run(sudoWithSecurityKey)}>
              Use security key or passkey
            </Button>
          )}
          {m?.totp && m.password && (
            <button
              type="button"
              className={styles.link}
              onClick={() => (setMethod(method === 'password' ? 'totp' : 'password'), setValue(''), setError(null))}
            >
              <DeviceMobileIcon size={12} /> {method === 'password' ? 'Use an authentication code' : 'Use your password'}
            </button>
          )}
        </div>
      </form>
    </Dialog>
  );
}
