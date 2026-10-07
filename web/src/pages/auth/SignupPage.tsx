import { useRef, useState } from 'react';
import { emailProblem, loginProblem, passwordProblem } from '../../api/auth';
import { ApiError } from '../../api/client';
import { invitationTarget, invitationTargetLabel } from '../invitations/model';
import { session } from '../../app/session';
import { getBoot } from '../../boot';
import { Link, navigate, returnTo } from '../../router';
import { Button } from '../../ui/Button';
import { Input } from '../../ui/Input';
import { CheckIcon, CircleSlashIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, Flash, messageOf, StateBlock, statusOf } from './AuthPage';
import { PasswordStrength } from './PasswordStrength';

type FieldName = 'email' | 'password' | 'login';
type Errors = Partial<Record<FieldName, string>>;

/** Map a 422 from `create_user` to per-field messages. */
export function signupFieldErrors(e: unknown, login: string): Errors {
  const out: Errors = {};
  if (!(e instanceof ApiError) || e.status !== 422) return out;
  const errs = (e.body as { errors?: { field?: string; code?: string; message?: string }[] } | null)?.errors ?? [];
  for (const err of errs) {
    const f = err.field as FieldName | undefined;
    if (f !== 'login' && f !== 'email' && f !== 'password') continue;
    if (err.code === 'already_exists') out[f] = f === 'login' ? `Username ${login} is not available.` : 'Email is invalid or already taken.';
    else if (err.code === 'missing_field') out[f] = `${f === 'login' ? 'Username' : f === 'email' ? 'Email' : 'Password'} is required.`;
    else if (err.message) out[f] = err.message.charAt(0).toUpperCase() + err.message.slice(1) + (err.message.endsWith('.') ? '' : '.');
    else out[f] = f === 'login' ? `Username ${login} is not available.` : f === 'email' ? 'Email is invalid or already taken.' : 'Password is invalid.';
  }
  return out;
}

/** `/signup`: create an account (email, password, username). */
export default function SignupPage() {
  const { config } = getBoot();
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [login, setLogin] = useState('');
  const [touched, setTouched] = useState<Partial<Record<FieldName, boolean>>>({});
  const [serverErrors, setServerErrors] = useState<Errors>({});
  const [notice, setNotice] = useState<string | null>(null);
  const [disabled, setDisabled] = useState(!config.signupEnabled);
  const [busy, setBusy] = useState(false);
  const target = returnTo();
  const invite = invitationTarget(target);
  const loginHref = target === '/' ? '/login' : `/login?return_to=${encodeURIComponent(target)}`;
  const refs = { email: useRef<HTMLInputElement>(null), password: useRef<HTMLInputElement>(null), login: useRef<HTMLInputElement>(null) };

  const client: Errors = {
    email: emailProblem(email) ?? undefined,
    password: passwordProblem(password) ?? undefined,
    login: loginProblem(login) ?? undefined,
  };
  const errorFor = (f: FieldName) => serverErrors[f] ?? (touched[f] ? client[f] : undefined);

  const submit = async () => {
    setTouched({ email: true, password: true, login: true });
    const first = (['email', 'password', 'login'] as const).find((f) => client[f]);
    if (first) return refs[first].current?.focus();
    setBusy(true);
    setNotice(null);
    setServerErrors({});
    try {
      await session.signup({ login: login.trim(), email: email.trim(), password });
      navigate(target, { replace: true });
    } catch (e) {
      const status = statusOf(e);
      // Closed instances disable the form; invite-only and domain rules explain themselves.
      if (status === 403 && /disabled/i.test(messageOf(e))) setDisabled(true);
      else if (status === 403) setNotice(invite ? `${messageOf(e)} Use the email address your invitation was sent to.` : messageOf(e));
      else if (status === 422) {
        const fields = signupFieldErrors(e, login.trim());
        setServerErrors(fields);
        const f = (['email', 'password', 'login'] as const).find((k) => fields[k]);
        if (f) refs[f].current?.focus();
        else setNotice(messageOf(e));
      } else setNotice(messageOf(e));
    } finally {
      setBusy(false);
    }
  };

  if (disabled) {
    return (
      <AuthLayout title={`Join ${config.siteName}`}>
        <StateBlock
          icon={CircleSlashIcon}
          title="Sign up is disabled"
          actions={
            <Button size="lg" variant="primary" onClick={() => navigate(loginHref)}>
              Sign in
            </Button>
          }
        >
          This instance doesn't allow new accounts. Ask a site administrator to create one for you.
        </StateBlock>
      </AuthLayout>
    );
  }

  const field = (f: FieldName) => ({
    id: `user_${f}`,
    ref: refs[f],
    size: 'lg' as const,
    invalid: !!errorFor(f),
    'aria-describedby': `${f}-help`,
    onBlur: () => setTouched((t) => ({ ...t, [f]: true })),
  });
  const loginOk = touched.login && login && !errorFor('login');

  return (
    <AuthLayout
      title={`Create your ${config.siteName} account`}
      banner={
        notice ? (
          <Flash tone="danger" onDismiss={() => setNotice(null)}>
            {notice}
          </Flash>
        ) : (
          invite && <Flash>Create an account to accept your invitation to {invitationTargetLabel(invite)}.</Flash>
        )
      }
      below={
        <>
          Already have an account? <Link to={loginHref}>Sign in →</Link>
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
          <label htmlFor="user_email" className={styles.labelRow}>
            <span>Email</span>
          </label>
          <Input
            {...field('email')}
            type="email"
            autoFocus
            autoComplete="email"
            spellCheck={false}
            value={email}
            onChange={(e) => {
              setEmail(e.target.value);
              setServerErrors((s) => ({ ...s, email: undefined }));
            }}
          />
          <Help id="email-help" error={errorFor('email')} />
        </div>
        <div className={styles.fieldGroup}>
          <label htmlFor="user_password" className={styles.labelRow}>
            <span>Password</span>
          </label>
          <Input
            {...field('password')}
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(e) => {
              setPassword(e.target.value);
              setServerErrors((s) => ({ ...s, password: undefined }));
            }}
          />
          {errorFor('password') ? <Help id="password-help" error={errorFor('password')} /> : <PasswordStrength id="password-help" password={password} context={[login, email.split('@')[0] ?? '']} />}
        </div>
        <div className={styles.fieldGroup}>
          <label htmlFor="user_login" className={styles.labelRow}>
            <span>Username</span>
          </label>
          <Input
            {...field('login')}
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            maxLength={39}
            value={login}
            trailing={loginOk ? <CheckIcon size={16} aria-label="Valid username" /> : undefined}
            onChange={(e) => {
              setLogin(e.target.value);
              setTouched((t) => ({ ...t, login: true }));
              setServerErrors((s) => ({ ...s, login: undefined }));
            }}
          />
          <Help id="login-help" error={errorFor('login')} hint="Alphanumeric characters or single hyphens; can't begin or end with a hyphen." />
        </div>
        <Button type="submit" variant="primary" size="lg" block loading={busy}>
          Create account
        </Button>
      </form>
    </AuthLayout>
  );
}

function Help({ id, error, hint }: { id: string; error?: string; hint?: string }) {
  if (error)
    return (
      <div id={id} className={styles.fieldError}>
        {error}
      </div>
    );
  return hint ? (
    <div id={id} className={styles.fieldHint}>
      {hint}
    </div>
  ) : null;
}
