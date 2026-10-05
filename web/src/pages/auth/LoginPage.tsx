import { useEffect, useRef, useState, type MouseEvent } from 'react';
import { listSsoProviders, ssoLoginHref, type SsoProvider } from '../../api/auth';
import { api } from '../../api/client';
import { returnTo } from '../../app/App';
import { invitationTarget, invitationTargetLabel } from '../invitations/model';
import { session } from '../../app/session';
import type { PublicSiteInfo } from '../../app/site';
import { getBoot, isMockMode } from '../../boot';
import { Link, navigate, useLocation } from '../../router';
import { Button } from '../../ui/Button';
import { Input } from '../../ui/Input';
import { ArrowLeftIcon, DeviceMobileIcon, ShieldLockIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, Divider, Flash, messageOf, statusOf } from './AuthPage';
import { TwoFactorForm, WrongCode } from './TwoFactorForm';

interface Errors {
  login?: string;
  password?: string;
}

type Notice = { tone: 'danger' | 'warning' | 'success' | 'info'; text: string } | null;

function initialNotice(search: string): Notice {
  const q = new URLSearchParams(search);
  const err = q.get('error');
  if (err) return { tone: 'danger', text: err };
  if (q.has('password_reset')) return { tone: 'success', text: 'Your password has been changed. Sign in with your new password.' };
  return null;
}

/** `/login`: password sign-in with a second-factor step and SSO providers. */
export default function LoginPage() {
  const { search } = useLocation();
  // A new query (`?error=` from SSO, `?password_reset`) starts a fresh form.
  return <Login key={search} search={search} />;
}

function Login({ search }: { search: string }) {
  const { config } = getBoot();
  const [login, setLogin] = useState('');
  const [password, setPassword] = useState('');
  const [errors, setErrors] = useState<Errors>({});
  const [notice, setNotice] = useState<Notice>(() => initialNotice(search));
  const [busy, setBusy] = useState(false);
  const [twoFactorToken, setTwoFactorToken] = useState<string | null>(null);
  const [providers, setProviders] = useState<SsoProvider[]>([]);
  const [ssoBusy, setSsoBusy] = useState<string | null>(null);
  const [site, setSite] = useState<PublicSiteInfo | null>(null);
  const [adminForm, setAdminForm] = useState(false);
  // Private mode (`/_bgh/site` stays public): explain why sign-in is needed.
  const privateMode = !!site?.private_mode;
  const passwordRef = useRef<HTMLInputElement>(null);
  const loginRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let live = true;
    listSsoProviders().then(
      (p) => live && setProviders(p),
      () => undefined,
    );
    api.get<PublicSiteInfo>('/_bgh/site').then(
      (s) => live && setSite(s),
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, []);

  // Captured at render: once the session flips, App itself redirects and the
  // URL loses its `return_to`.
  const target = returnTo(search);
  const invite = invitationTarget(target);
  const done = () => navigate(target, { replace: true });

  const submit = async () => {
    const next: Errors = {};
    if (!login.trim()) next.login = 'Enter your username or email address.';
    if (!password) next.password = 'Enter your password.';
    setErrors(next);
    if (next.login) return loginRef.current?.focus();
    if (next.password) return passwordRef.current?.focus();
    setBusy(true);
    setNotice(null);
    try {
      const step = await session.login(login.trim(), password);
      if (step) {
        setTwoFactorToken(step.twoFactorToken);
        return;
      }
      done();
    } catch (e) {
      const status = statusOf(e);
      if (status === 429) setNotice({ tone: 'warning', text: messageOf(e) || 'Too many failed login attempts. Please try again later.' });
      else if (status === 422 || status === 401) {
        setNotice({ tone: 'danger', text: 'Incorrect username or password.' });
        setPassword('');
        passwordRef.current?.focus();
      } else setNotice({ tone: 'danger', text: messageOf(e) });
    } finally {
      setBusy(false);
    }
  };

  const verify = async (code: string) => {
    try {
      await session.verifyTwoFactor(twoFactorToken!, code);
      done();
    } catch (e) {
      const status = statusOf(e);
      if (status === 422) throw new WrongCode('Incorrect two-factor code. Try again.', { cause: e });
      if (status === 401 || status === 429) {
        // The pending login expired or was locked: start over.
        setTwoFactorToken(null);
        setPassword('');
        setNotice({ tone: status === 429 ? 'warning' : 'danger', text: messageOf(e) });
        return;
      }
      throw new Error(messageOf(e), { cause: e });
    }
  };

  const startSso = (id: string, href: string) => (e: MouseEvent<HTMLAnchorElement>) => {
    setSsoBusy(id);
    if (!isMockMode()) return; // full page navigation to the provider
    // The mock backend lives in this page, so emulate the redirect round trip.
    e.preventDefault();
    void api
      .get(href)
      .then(() => session.refreshBoot())
      .then(done, (err: unknown) => {
        setSsoBusy(null);
        setNotice({ tone: 'danger', text: messageOf(err) });
      });
  };

  // Until /_bgh/site answers (or when it fails) the form is shown.
  const ldap = !!site?.ldap;
  const showForm = !site || site.password_login || ldap || adminForm;
  const usernameLabel = ldap && !site?.password_login ? 'LDAP username' : 'Username or email address';
  const saml = site?.saml;
  const samlHref = saml && `${saml.login_url}?return_to=${encodeURIComponent(target)}`;

  if (twoFactorToken) {
    return (
      <AuthLayout
        title="Two-factor authentication"
        hero={
          <span className={styles.twoFactorIcon}>
            <DeviceMobileIcon size={24} />
          </span>
        }
        below={
          <span className={styles.small}>
            Lost access to your device and recovery codes?{' '}
            <Link to="/password_reset">Reset your password</Link> or contact your site administrator.
          </span>
        }
      >
        <TwoFactorForm
          verify={verify}
          footer={
            <div className={`${styles.small} ${styles.centered}`}>
              <button
                type="button"
                className={styles.linkButton}
                onClick={() => {
                  setTwoFactorToken(null);
                  setPassword('');
                }}
              >
                <ArrowLeftIcon size={12} /> Back to sign in
              </button>
            </div>
          }
        />
      </AuthLayout>
    );
  }

  return (
    <AuthLayout
      title={`Sign in to ${config.siteName}`}
      banner={
        notice ? (
          <Flash tone={notice.tone} onDismiss={() => setNotice(null)}>
            {notice.text}
          </Flash>
        ) : (
          invite && <Flash>Sign in{config.signupEnabled ? ' or create an account' : ''} to accept your invitation to {invitationTargetLabel(invite)}.</Flash>
        )
      }
      below={
        config.signupEnabled ? (
          <>
            New to {config.siteName}? <Link to={`/signup${target === '/' ? '' : `?return_to=${encodeURIComponent(target)}`}`}>Create an account</Link>
          </>
        ) : undefined
      }
    >
      {privateMode && (
        <p className={`${styles.small} ${styles.muted}`} style={{ margin: '0 0 14px' }} data-testid="private-mode-note">
          {config.siteName} is private. Sign in to see its repositories, people and organizations.
        </p>
      )}
      {isMockMode() && (
        <p className={`${styles.small} ${styles.muted}`} style={{ margin: '0 0 14px' }}>
          Any credentials work. Passwords <code>wrong</code>, <code>throttle</code> and <code>2fa</code> (code <code>123456</code>) try the other paths.
        </p>
      )}
      {!showForm && (
        <p className={`${styles.small} ${styles.muted}`} style={{ margin: '0 0 4px' }}>
          Password sign-in is disabled. Sign in with single sign-on.
        </p>
      )}
      {showForm && (
        <form
          className={styles.form}
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
        <div className={styles.fieldGroup}>
          <div className={styles.labelRow}>
            <label htmlFor="login_field">{usernameLabel}</label>
          </div>
          <Input
            id="login_field"
            ref={loginRef}
            size="lg"
            autoFocus
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            value={login}
            invalid={!!errors.login}
            aria-describedby={errors.login ? 'login-error' : undefined}
            onChange={(e) => {
              setLogin(e.target.value);
              if (errors.login) setErrors({ ...errors, login: undefined });
            }}
          />
          {errors.login && (
            <div id="login-error" className={styles.fieldError}>
              {errors.login}
            </div>
          )}
        </div>
        <div className={styles.fieldGroup}>
          <div className={styles.labelRow}>
            <label htmlFor="password">Password</label>
            {site?.password_login !== false && (
              <Link to="/password_reset" className={styles.small}>
                Forgot password?
              </Link>
            )}
          </div>
          <Input
            id="password"
            ref={passwordRef}
            size="lg"
            type="password"
            autoComplete="current-password"
            value={password}
            invalid={!!errors.password}
            aria-describedby={errors.password ? 'password-error' : undefined}
            onChange={(e) => {
              setPassword(e.target.value);
              if (errors.password) setErrors({ ...errors, password: undefined });
            }}
          />
          {errors.password && (
            <div id="password-error" className={styles.fieldError}>
              {errors.password}
            </div>
          )}
        </div>
        <Button type="submit" variant="primary" size="lg" block loading={busy}>
          Sign in
        </Button>
        </form>
      )}
      {(providers.length > 0 || samlHref) && (
        <>
          {showForm && <Divider />}
          <div className={styles.sso}>
            {samlHref && (
              <a href={samlHref} className={styles.ssoButton} aria-busy={ssoBusy === 'saml'} onClick={startSso('saml', samlHref)}>
                <ShieldLockIcon size={16} />
                Sign in with {saml!.display_name}
              </a>
            )}
            {providers.map((p) => (
              <a key={p.id} href={ssoLoginHref(p.id, target)} className={styles.ssoButton} aria-busy={ssoBusy === p.id} onClick={startSso(p.id, ssoLoginHref(p.id, target))}>
                <ShieldLockIcon size={16} />
                Sign in with {p.name}
              </a>
            ))}
          </div>
        </>
      )}
      {!showForm && site?.password_login_admin_exempt && (
        <div className={`${styles.small} ${styles.centered}`} style={{ marginTop: 14 }}>
          <button type="button" className={styles.linkButton} onClick={() => setAdminForm(true)}>
            Site administrator sign-in
          </button>
        </div>
      )}
    </AuthLayout>
  );
}
