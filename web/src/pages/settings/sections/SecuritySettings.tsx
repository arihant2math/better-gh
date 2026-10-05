import { observer } from 'mobx-react-lite';
import { useEffect, useId, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { invalidate } from '../../../api/cache';
import {
  KEYS,
  MIN_PASSWORD_LEN,
  changePassword,
  disableTwoFactor,
  enableTotp,
  getTwoFactor,
  regenerateRecoveryCodes,
  startTotp,
  useEditableResource,
  type TotpSetup,
} from '../../../api/userSettings';
import { session } from '../../../app/session';
import {
  Banner,
  ButtonRow,
  Checkbox,
  CopyButton,
  FormStack,
  PageHeader,
  Pill,
  Section,
  apiFieldErrors,
  downloadText,
  errorMessage,
  type FieldErrors,
} from '../../../components/settings/kit';
import { Button } from '../../../ui/Button';
import { Dialog } from '../../../ui/Dialog';
import { Box, Skeleton } from '../../../ui/EmptyState';
import { AlertIcon, DeviceMobileIcon, DownloadIcon, KeyIcon, ShieldCheckIcon } from '../../../ui/icons';
import { Field, Input } from '../../../ui/Input';
import { RelativeTime } from '../../../ui/RelativeTime';
import { toast } from '../../../ui/Toast';
import { encodeQr, qrPath } from '../qr';
import styles from './userSettings.module.css';

export default function SecuritySettings() {
  return (
    <>
      <PageHeader title="Password and authentication" description="Keep your account secure with a strong password and a second factor." />
      <PasswordSection />
      <TwoFactorSection />
    </>
  );
}

// ------------------------------------------------------------------ password

interface PwForm {
  current: string;
  password: string;
  confirm: string;
}

/** Client-side checks before PUT /_bgh/user/password. */
export function validatePassword(f: PwForm): FieldErrors {
  const e: FieldErrors = {};
  if (!f.current) e.current_password = 'Enter your current password';
  const n = [...f.password].length;
  if (n < MIN_PASSWORD_LEN) e.password = `Password must be at least ${MIN_PASSWORD_LEN} characters`;
  else if (n > 1024) e.password = 'Password is too long (maximum is 1024 characters)';
  else if (f.password === f.current) e.password = 'New password must be different from your current password';
  if (!e.password && f.confirm !== f.password) e.confirm = "Passwords don't match";
  return e;
}

function PasswordSection() {
  const [f, setF] = useState<PwForm>({ current: '', password: '', confirm: '' });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [general, setGeneral] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const id = { cur: useId(), pw: useId(), confirm: useId() };
  const set = (k: keyof PwForm, v: string) => {
    setF((x) => ({ ...x, [k]: v }));
    const field = k === 'current' ? 'current_password' : k;
    if (errors[field]) setErrors((e) => ({ ...e, [field]: undefined }));
  };
  const submit = async (ev: FormEvent) => {
    ev.preventDefault();
    if (busy) return;
    const errs = validatePassword(f);
    setErrors(errs);
    setGeneral(null);
    if (Object.values(errs).some(Boolean)) return;
    setBusy(true);
    try {
      await changePassword(f.current, f.password);
      setF({ current: '', password: '', confirm: '' });
      invalidate(KEYS.sessions);
      toast({ kind: 'success', title: 'Password changed', description: 'Your other sessions have been signed out.' });
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setErrors(fields);
      if (!Object.values(fields).some(Boolean)) setGeneral(message);
    } finally {
      setBusy(false);
    }
  };
  const strength = passwordStrength(f.password);
  return (
    <Section title="Change password" description="Changing your password signs you out of all other sessions and sends a confirmation email.">
      <form onSubmit={(e) => void submit(e)} noValidate aria-label="Change password">
        <FormStack>
          {general && <Banner tone="danger">{general}</Banner>}
          {/* Lets password managers associate the change with the account. */}
          <input type="text" name="username" autoComplete="username" value={session.user?.login ?? ''} readOnly hidden />
          <Field label="Old password" htmlFor={id.cur} error={errors.current_password}>
            <Input id={id.cur} type="password" autoComplete="current-password" value={f.current} onChange={(e) => set('current', e.target.value)} invalid={!!errors.current_password} />
          </Field>
          <Field label="New password" htmlFor={id.pw} error={errors.password} hint={f.password ? strength.label : `At least ${MIN_PASSWORD_LEN} characters. A passphrase of a few words works well.`}>
            <Input id={id.pw} type="password" autoComplete="new-password" value={f.password} onChange={(e) => set('password', e.target.value)} invalid={!!errors.password} />
          </Field>
          {f.password && (
            <div className={styles.meter} aria-hidden>
              {[0, 1, 2, 3].map((i) => (
                <span key={i} data-on={i < strength.score || undefined} data-level={strength.score} />
              ))}
            </div>
          )}
          <Field label="Confirm new password" htmlFor={id.confirm} error={errors.confirm}>
            <Input id={id.confirm} type="password" autoComplete="new-password" value={f.confirm} onChange={(e) => set('confirm', e.target.value)} invalid={!!errors.confirm} />
          </Field>
          <ButtonRow>
            <Button type="submit" variant="primary" loading={busy}>
              Update password
            </Button>
          </ButtonRow>
        </FormStack>
      </form>
    </Section>
  );
}

/** Rough strength estimate for the meter (length + character classes). */
export function passwordStrength(pw: string): { score: number; label: string } {
  if (!pw) return { score: 0, label: '' };
  const classes = [/[a-z]/, /[A-Z]/, /\d/, /[^A-Za-z0-9]/].filter((r) => r.test(pw)).length;
  const n = [...pw].length;
  let score = n < MIN_PASSWORD_LEN ? 0 : n < 12 ? 1 : n < 16 ? 2 : 3;
  if (classes >= 3 && score > 0) score++;
  score = Math.min(4, score);
  const label = ['Too short', 'Weak', 'Fair', 'Good', 'Strong'][score]!;
  return { score, label: `Strength: ${label}` };
}

// ------------------------------------------------------------------ 2FA

const TwoFactorSection = observer(function TwoFactorSection() {
  const status = useEditableResource(KEYS.twoFactor, getTwoFactor);
  const [setup, setSetup] = useState<TotpSetup | null>(null);
  const [starting, setStarting] = useState(false);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [prompt, setPrompt] = useState<'disable' | 'regenerate' | null>(null);

  const begin = async () => {
    setStarting(true);
    try {
      setSetup(await startTotp());
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setStarting(false);
    }
  };

  const s = status.data;
  return (
    <Section
      title={
        <span className={styles.titleRow}>
          Two-factor authentication {s && (s.enabled ? <Pill tone="success">Enabled</Pill> : <Pill>Disabled</Pill>)}
        </span>
      }
      description="Two-factor authentication adds an additional layer of security by requiring a code from your authenticator app when you sign in."
    >
      {status.error && !s ? (
        <Banner tone="danger">{errorMessage(status.error)}</Banner>
      ) : !s ? (
        <Skeleton height={72} />
      ) : codes ? (
        <RecoveryCodes
          codes={codes}
          onDone={() => {
            setCodes(null);
            void status.refresh();
          }}
        />
      ) : setup ? (
        <TotpSetupPanel
          setup={setup}
          onCancel={() => setSetup(null)}
          onEnabled={(c) => {
            setSetup(null);
            setCodes(c);
            status.update(() => ({ enabled: true, enabled_at: new Date().toISOString(), recovery_codes_remaining: c.length }));
            invalidate(KEYS.me);
          }}
        />
      ) : s.enabled ? (
        <Box>
          <div className={styles.factorRow}>
            <DeviceMobileIcon size={20} />
            <div className={styles.grow}>
              <div className={styles.strong}>Authenticator app</div>
              <div className={styles.muted}>
                Configured {s.enabled_at && <RelativeTime date={s.enabled_at} />}. Use codes from your authenticator app to sign in.
              </div>
            </div>
            <Pill tone="success">Configured</Pill>
          </div>
          <div className={styles.factorRow}>
            <KeyIcon size={20} />
            <div className={styles.grow}>
              <div className={styles.strong}>Recovery codes</div>
              <div className={styles.muted} data-testid="recovery-remaining">
                {s.recovery_codes_remaining} of 10 unused. Use them to sign in if you lose access to your device.
              </div>
              {s.recovery_codes_remaining <= 3 && (
                <div className={styles.warnText}>
                  <AlertIcon size={14} /> You are running low on recovery codes. Generate new ones.
                </div>
              )}
            </div>
            <Button size="sm" onClick={() => setPrompt('regenerate')}>
              Regenerate
            </Button>
          </div>
          <div className={styles.factorRow}>
            <ShieldCheckIcon size={20} />
            <div className={styles.grow}>
              <div className={styles.strong}>Disable two-factor authentication</div>
              <div className={styles.muted}>Your account will only be protected by your password.</div>
            </div>
            <Button size="sm" variant="danger" onClick={() => setPrompt('disable')}>
              Disable
            </Button>
          </div>
        </Box>
      ) : (
        <Box padded>
          <div className={styles.factorRow}>
            <ShieldCheckIcon size={24} />
            <div className={styles.grow}>
              <div className={styles.strong}>Two-factor authentication is not enabled yet.</div>
              <div className={styles.muted}>Use an authenticator app such as 1Password, Authy or Google Authenticator. Accounts with 2FA must use tokens for Git over HTTPS.</div>
            </div>
            <Button variant="primary" loading={starting} onClick={() => void begin()}>
              Enable two-factor authentication
            </Button>
          </div>
        </Box>
      )}
      <PasswordPrompt
        open={prompt === 'disable'}
        title="Disable two-factor authentication"
        confirmLabel="Disable"
        danger
        description="Confirm your password to turn off two-factor authentication. Your recovery codes will stop working."
        onClose={() => setPrompt(null)}
        onSubmit={async (pw) => {
          await disableTwoFactor(pw);
          status.update(() => ({ enabled: false, enabled_at: null, recovery_codes_remaining: 0 }));
          invalidate(KEYS.me);
          toast({ kind: 'success', title: 'Two-factor authentication disabled' });
        }}
      />
      <PasswordPrompt
        open={prompt === 'regenerate'}
        title="Regenerate recovery codes"
        confirmLabel="Generate new codes"
        description="Your old recovery codes will stop working immediately."
        onClose={() => setPrompt(null)}
        onSubmit={async (pw) => {
          const r = await regenerateRecoveryCodes(pw);
          setCodes(r.recovery_codes);
          status.update((x) => ({ ...x, recovery_codes_remaining: r.recovery_codes.length }));
        }}
      />
    </Section>
  );
});

/** Group a base32 secret in blocks of four for reading aloud / typing. */
export function groupSecret(secret: string): string {
  return secret.replace(/\s+/g, '').replace(/(.{4})(?=.)/g, '$1 ');
}

function QrSvg({ text, label }: { text: string; label: string }) {
  const svg = useMemo(() => {
    const qr = encodeQr(text);
    return { d: qrPath(qr), n: qr.size + 8 };
  }, [text]);
  return (
    <div className={styles.qr} data-theme-scope="light">
      <svg viewBox={`0 0 ${svg.n} ${svg.n}`} width={184} height={184} role="img" aria-label={label} shapeRendering="crispEdges" data-testid="totp-qr">
        <rect width={svg.n} height={svg.n} className={styles.qrBg} />
        <path d={svg.d} className={styles.qrFg} />
      </svg>
    </div>
  );
}

function TotpSetupPanel({ setup, onCancel, onEnabled }: { setup: TotpSetup; onCancel: () => void; onEnabled: (codes: string[]) => void }) {
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const codeId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => inputRef.current?.focus(), []);

  const submit = async (value = code, ev?: FormEvent) => {
    ev?.preventDefault();
    if (busy) return;
    const c = value.replace(/\s+/g, '');
    if (!/^\d{6}$/.test(c)) return setError('Enter the 6-digit code from your authenticator app');
    setBusy(true);
    setError(null);
    try {
      const r = await enableTotp(c);
      toast({ kind: 'success', title: 'Two-factor authentication enabled' });
      onEnabled(r.recovery_codes);
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setError(fields.code ?? message);
      setCode('');
      inputRef.current?.focus();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Box padded>
      <div className={styles.setupGrid}>
        <div>
          <h3 className={styles.stepTitle}>1. Scan the QR code</h3>
          <p className={styles.muted}>Use an authenticator app or browser extension to scan.</p>
          <QrSvg text={setup.otpauth_uri} label="QR code for your authenticator app" />
          <p className={styles.muted}>Unable to scan? Enter this setup key instead:</p>
          <div className={styles.secretRow}>
            <code className={styles.secret} data-testid="totp-secret">
              {groupSecret(setup.secret)}
            </code>
            <CopyButton value={setup.secret} label="Copy" />
          </div>
        </div>
        <form onSubmit={(e) => void submit(code, e)} noValidate className={styles.stepForm}>
          <h3 className={styles.stepTitle}>2. Verify the code from the app</h3>
          <Field label="Authentication code" htmlFor={codeId} error={error} hint="Enter the 6-digit code shown in your app.">
            <Input
              ref={inputRef}
              id={codeId}
              className={styles.codeInput}
              inputMode="numeric"
              autoComplete="one-time-code"
              placeholder="XXXXXX"
              maxLength={7}
              value={code}
              invalid={!!error}
              onChange={(e) => {
                const v = e.target.value.replace(/[^\d ]/g, '');
                setCode(v);
                setError(null);
                if (v.replace(/\s/g, '').length === 6) void submit(v);
              }}
            />
          </Field>
          <ButtonRow>
            <Button type="submit" variant="primary" loading={busy}>
              Verify and enable
            </Button>
            <Button onClick={onCancel} disabled={busy}>
              Cancel
            </Button>
          </ButtonRow>
        </form>
      </div>
    </Box>
  );
}

export function recoveryCodesText(codes: readonly string[], login: string, site = 'Better GitHub', at = new Date()): string {
  return [
    `${site} recovery codes for @${login}`,
    `Generated ${at.toISOString().slice(0, 10)}`,
    '',
    ...codes,
    '',
    'Each code can be used once to sign in if you lose access to your authenticator app.',
    'Keep these somewhere safe, such as a password manager.',
    '',
  ].join('\n');
}

function printCodes(text: string) {
  const frame = document.createElement('iframe');
  frame.style.position = 'fixed';
  frame.style.width = '0';
  frame.style.height = '0';
  frame.style.border = '0';
  document.body.appendChild(frame);
  const doc = frame.contentDocument!;
  const pre = doc.createElement('pre');
  pre.textContent = text;
  pre.style.font = '14px/1.6 ui-monospace, monospace';
  doc.body.appendChild(pre);
  frame.contentWindow!.focus();
  frame.contentWindow!.print();
  setTimeout(() => frame.remove(), 1000);
}

function RecoveryCodes({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  const [saved, setSaved] = useState(false);
  const login = session.user?.login ?? 'you';
  const text = recoveryCodesText(codes, login);
  return (
    <Box padded>
      <FormStack wide>
        <Banner tone="warning" icon={AlertIcon}>
          <strong>Save your recovery codes now.</strong> They are shown only once. If you lose your device and your recovery codes, you will lose access to your
          account.
        </Banner>
        <ul className={styles.codes} aria-label="Recovery codes" data-testid="recovery-codes">
          {codes.map((c) => (
            <li key={c}>{c}</li>
          ))}
        </ul>
        <ButtonRow>
          <Button
            leadingIcon={DownloadIcon}
            onClick={() => {
              downloadText(`${login}-recovery-codes.txt`, text);
              setSaved(true);
            }}
          >
            Download
          </Button>
          <CopyButton value={codes.join('\n')} size="md" label="Copy" />
          <Button onClick={() => printCodes(text)}>Print</Button>
        </ButtonRow>
        <Checkbox checked={saved} onChange={setSaved} label="I have saved my recovery codes" />
        <ButtonRow>
          <Button variant="primary" disabled={!saved} onClick={onDone}>
            Done
          </Button>
        </ButtonRow>
      </FormStack>
    </Box>
  );
}

function PasswordPrompt({
  open,
  title,
  description,
  confirmLabel,
  danger,
  onClose,
  onSubmit,
}: {
  open: boolean;
  title: string;
  description: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  onClose: () => void;
  onSubmit: (password: string) => Promise<void>;
}) {
  const [pw, setPw] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const id = useId();
  useEffect(() => {
    if (open) {
      setPw('');
      setError(null);
    }
  }, [open]);
  const submit = async (ev?: FormEvent) => {
    ev?.preventDefault();
    if (busy) return;
    if (!pw) return setError('Enter your password');
    setBusy(true);
    setError(null);
    try {
      await onSubmit(pw);
      onClose();
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
      title={title}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant={danger ? 'danger' : 'primary'} loading={busy} onClick={() => void submit()}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <form onSubmit={(e) => void submit(e)} noValidate>
        <FormStack>
          <p className={styles.muted}>{description}</p>
          <Field label="Password" htmlFor={id} error={error}>
            <Input id={id} type="password" autoComplete="current-password" autoFocus value={pw} invalid={!!error} onChange={(e) => setPw(e.target.value)} />
          </Field>
        </FormStack>
      </form>
    </Dialog>
  );
}
