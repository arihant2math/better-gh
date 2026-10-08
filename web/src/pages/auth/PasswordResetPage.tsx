import { useRef, useState } from 'react';
import { checkPasswordReset, passwordProblem, requestPasswordReset, resetPassword, type ResetInfo } from '../../api/auth';
import { validationErrors } from '../../api/errors';
import { session } from '../../app/session';
import { Link, navigate, useLocation } from '../../router';
import { Button } from '../../ui/Button';
import { Input } from '../../ui/Input';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon, MailIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, Flash, messageOf, StateBlock, statusOf, useLoad } from './AuthPage';
import { PasswordStrength } from './PasswordStrength';

/** `/password_reset` (request a link) and `/password_reset/:token` (choose a new password). */
export default function PasswordResetPage() {
  const { pathname } = useLocation();
  const token = decodeURIComponent(pathname.match(/^\/password_reset\/([^/]+)/)?.[1] ?? '');
  return token ? <ChangePassword key={token} token={token} /> : <RequestReset />;
}

function RequestReset() {
  const [ident, setIdent] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLInputElement>(null);

  const submit = async () => {
    if (!ident.trim()) {
      setError('Enter your email address or username.');
      return ref.current?.focus();
    }
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await requestPasswordReset(ident);
      setSent(ident.trim());
    } catch (e) {
      if (statusOf(e) === 422) setError('Enter your email address or username.');
      else setNotice(messageOf(e));
    } finally {
      setBusy(false);
    }
  };

  if (sent) {
    return (
      <AuthLayout title="Reset your password">
        <StateBlock
          icon={MailIcon}
          tone="accent"
          title="Check your email"
          actions={
            <Button size="lg" variant="primary" block onClick={() => navigate('/login')}>
              Return to sign in
            </Button>
          }
        >
          If <strong>{sent}</strong> matches an account, we sent a link to reset your password. It expires in one hour. If it doesn't
          show up within a few minutes, check your spam folder.
        </StateBlock>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout
      title="Reset your password"
      banner={notice && <Flash tone={/too many/i.test(notice) ? 'warning' : 'danger'} onDismiss={() => setNotice(null)}>{notice}</Flash>}
      below={
        <>
          Remembered it? <Link to="/login">Sign in</Link>
        </>
      }
    >
      <form
        className={styles.form}
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className={styles.fieldGroup}>
          <label htmlFor="reset_ident" className={styles.labelRow}>
            <span>Email address or username</span>
          </label>
          <Input
            id="reset_ident"
            ref={ref}
            size="lg"
            autoFocus
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            value={ident}
            invalid={!!error}
            aria-describedby="reset-help"
            onChange={(e) => {
              setIdent(e.target.value);
              setError(null);
            }}
          />
          {error ? (
            <div id="reset-help" className={styles.fieldError}>
              {error}
            </div>
          ) : (
            <div id="reset-help" className={styles.fieldHint}>
              We'll email a password reset link to the account's primary address.
            </div>
          )}
        </div>
        <Button type="submit" variant="primary" size="lg" block loading={busy}>
          Send password reset email
        </Button>
      </form>
    </AuthLayout>
  );
}

interface ResetErrors {
  password?: string;
  confirm?: string;
  otp?: string;
}

function ChangePassword({ token }: { token: string }) {
  const [info, setInfo] = useLoad<ResetInfo>(token, () => checkPasswordReset(token));
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [otp, setOtp] = useState('');
  const [errors, setErrors] = useState<ResetErrors>({});
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const refs = { password: useRef<HTMLInputElement>(null), confirm: useRef<HTMLInputElement>(null), otp: useRef<HTMLInputElement>(null) };

  if (info.status === 'loading') {
    return (
      <AuthLayout title="Change your password">
        <div className={styles.form} aria-busy>
          <Skeleton height={14} width="40%" />
          <Skeleton height={36} />
          <Skeleton height={14} width="50%" />
          <Skeleton height={36} />
        </div>
      </AuthLayout>
    );
  }

  if (info.status === 'error') {
    const invalid = statusOf(info.error) === 404;
    return (
      <AuthLayout title="Change your password">
        <StateBlock
          icon={AlertIcon}
          tone="danger"
          title={invalid ? 'This link is invalid or has expired' : 'Something went wrong'}
          actions={
            <Button size="lg" variant="primary" block onClick={() => navigate('/password_reset')}>
              Send a new reset email
            </Button>
          }
        >
          {invalid ? 'Password reset links can be used once and expire after an hour. Request a new one to continue.' : messageOf(info.error)}
        </StateBlock>
      </AuthLayout>
    );
  }

  const { login, two_factor_required: needOtp } = info.data;

  const submit = async () => {
    const next: ResetErrors = {};
    const p = passwordProblem(password);
    if (p) next.password = p;
    else if (confirm !== password) next.confirm = "Passwords don't match.";
    if (needOtp && !otp.trim()) next.otp = 'Enter a two-factor code or a recovery code.';
    setErrors(next);
    const first = (['password', 'confirm', 'otp'] as const).find((f) => next[f]);
    if (first) return refs[first].current?.focus();
    setBusy(true);
    setNotice(null);
    try {
      await resetPassword(token, password, needOtp ? otp.trim() : undefined);
      // Every session was signed out by the reset; drop ours too.
      if (session.user) await session.logout();
      navigate('/login?password_reset=1', { replace: true });
    } catch (e) {
      const status = statusOf(e);
      if (status === 404) return setInfo({ status: 'error', error: e });
      if (status === 422) {
        const errs = validationErrors(e);
        const f: ResetErrors = {};
        for (const er of errs) {
          if (er.field === 'password') f.password = `Password ${er.message?.replace(/^password\s*/i, '') ?? 'is invalid'}.`;
          if (er.field === 'otp') f.otp = 'That two-factor code is not valid. Try again.';
        }
        if (f.password || f.otp) {
          setErrors(f);
          if (f.otp) setOtp('');
          return (f.password ? refs.password : refs.otp).current?.focus();
        }
      }
      setNotice(messageOf(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <AuthLayout
      title="Change your password"
      subtitle={
        <>
          for <strong>@{login}</strong>
        </>
      }
      banner={notice && <Flash tone={/too many/i.test(notice) ? 'warning' : 'danger'} onDismiss={() => setNotice(null)}>{notice}</Flash>}
    >
      <form
        className={styles.form}
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <input type="text" name="username" autoComplete="username" value={login} readOnly hidden />
        <div className={styles.fieldGroup}>
          <label htmlFor="new_password" className={styles.labelRow}>
            <span>New password</span>
          </label>
          <Input
            id="new_password"
            ref={refs.password}
            size="lg"
            type="password"
            autoFocus
            autoComplete="new-password"
            value={password}
            invalid={!!errors.password}
            aria-describedby="new-password-help"
            onChange={(e) => {
              setPassword(e.target.value);
              setErrors((x) => ({ ...x, password: undefined, confirm: undefined }));
            }}
          />
          {errors.password ? (
            <div id="new-password-help" className={styles.fieldError}>
              {errors.password}
            </div>
          ) : (
            <PasswordStrength id="new-password-help" password={password} context={[login]} />
          )}
        </div>
        <div className={styles.fieldGroup}>
          <label htmlFor="confirm_password" className={styles.labelRow}>
            <span>Confirm password</span>
          </label>
          <Input
            id="confirm_password"
            ref={refs.confirm}
            size="lg"
            type="password"
            autoComplete="new-password"
            value={confirm}
            invalid={!!errors.confirm}
            aria-describedby={errors.confirm ? 'confirm-help' : undefined}
            onChange={(e) => {
              setConfirm(e.target.value);
              setErrors((x) => ({ ...x, confirm: undefined }));
            }}
          />
          {errors.confirm && (
            <div id="confirm-help" className={styles.fieldError}>
              {errors.confirm}
            </div>
          )}
        </div>
        {needOtp && (
          <div className={styles.fieldGroup}>
            <label htmlFor="reset_otp" className={styles.labelRow}>
              <span>Two-factor code</span>
            </label>
            <Input
              id="reset_otp"
              ref={refs.otp}
              size="lg"
              autoComplete="one-time-code"
              spellCheck={false}
              placeholder="123456 or a recovery code"
              value={otp}
              invalid={!!errors.otp}
              aria-describedby="otp-help"
              onChange={(e) => {
                setOtp(e.target.value);
                setErrors((x) => ({ ...x, otp: undefined }));
              }}
            />
            {errors.otp ? (
              <div id="otp-help" className={styles.fieldError}>
                {errors.otp}
              </div>
            ) : (
              <div id="otp-help" className={styles.fieldHint}>
                Your account has two-factor authentication. Enter a code from your app or a recovery code.
              </div>
            )}
          </div>
        )}
        <Button type="submit" variant="primary" size="lg" block loading={busy}>
          Change password
        </Button>
        <p className={`${styles.small} ${styles.muted} ${styles.centered}`} style={{ margin: 0 }}>
          You'll be signed out on every device.
        </p>
      </form>
    </AuthLayout>
  );
}
