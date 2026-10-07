import { useRef, useState, type ReactNode } from 'react';
import { Button } from '../../ui/Button';
import { Input } from '../../ui/Input';
import { DeviceMobileIcon, KeyIcon, ShieldLockIcon } from '../../ui/icons';
import { authStyles as styles, OtpInput } from './AuthPage';

export interface TwoFactorFormProps {
  /** Resolves on success; reject with a message for an inline error. */
  verify: (code: string) => Promise<void>;
  /** Shown under the form ("Back to sign in"). */
  footer?: ReactNode;
  /** The account has security keys: offer them (runs the browser prompt). */
  securityKey?: () => Promise<void>;
}

/** Thrown by `verify` for a wrong code (shown under the input, which is cleared). */
export class WrongCode extends Error {}

/**
 * Second factor step: 6-digit TOTP that submits itself on the sixth digit,
 * or a recovery code (`xxxxx-xxxxx`).
 */
export function TwoFactorForm({ verify, footer, securityKey }: TwoFactorFormProps) {
  const [recovery, setRecovery] = useState(false);
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);

  const submit = async (value = code) => {
    const v = value.trim();
    if (inFlight.current) return;
    if (!recovery && !/^\d{6}$/.test(v)) return setError('Enter the 6-digit code from your authenticator app.');
    if (recovery && v.length < 8) return setError('Enter one of your recovery codes.');
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      await verify(v);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Two-factor authentication failed.');
      if (e instanceof WrongCode) setCode('');
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const runSecurityKey = async () => {
    if (!securityKey || inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      await securityKey();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Security key verification failed.');
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const toggle = () => {
    setRecovery((r) => !r);
    setCode('');
    setError(null);
  };

  return (
    <form
      className={styles.form}
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className={styles.fieldGroup}>
        <label htmlFor={recovery ? 'recovery-code' : 'otp'} className={styles.labelRow}>
          <span>{recovery ? 'Recovery code' : 'Authentication code'}</span>
        </label>
        {recovery ? (
          <Input
            id="recovery-code"
            size="lg"
            autoFocus
            autoComplete="off"
            spellCheck={false}
            placeholder="xxxxx-xxxxx"
            value={code}
            invalid={!!error}
            disabled={busy}
            aria-describedby="tf-help"
            onChange={(e) => setCode(e.target.value)}
          />
        ) : (
          <OtpInput id="otp" value={code} onChange={setCode} onComplete={(v) => void submit(v)} invalid={!!error} disabled={busy} />
        )}
        {error ? (
          <div id="tf-help" className={styles.fieldError} role="alert">
            {error}
          </div>
        ) : (
          <div id="tf-help" className={styles.fieldHint}>
            {recovery
              ? 'Each recovery code can be used once. You saved them when you set up two-factor authentication.'
              : 'Open your two-factor authenticator (TOTP) app to view your code.'}
          </div>
        )}
      </div>
      <Button type="submit" variant="primary" size="lg" block loading={busy}>
        Verify
      </Button>
      {securityKey && (
        <Button size="lg" block leadingIcon={ShieldLockIcon} disabled={busy} onClick={() => void runSecurityKey()} data-testid="use-security-key">
          Use security key
        </Button>
      )}
      <div className={`${styles.small} ${styles.centered}`}>
        <button type="button" className={styles.linkButton} onClick={toggle}>
          {recovery ? (
            <>
              <DeviceMobileIcon size={12} /> Use your authenticator app
            </>
          ) : (
            <>
              <KeyIcon size={12} /> Use a recovery code
            </>
          )}
        </button>
      </div>
      {footer}
    </form>
  );
}
