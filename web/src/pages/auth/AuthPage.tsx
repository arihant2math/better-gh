import { useState } from 'react';
import { ApiError } from '../../api/client';
import { session } from '../../app/session';
import { getBoot, isMockMode } from '../../boot';
import { Link, navigate } from '../../router';
import { Button } from '../../ui/Button';
import { Field, Input } from '../../ui/Input';
import styles from './AuthPage.module.css';

/** Sign in / sign up. Posts to /_bgh/auth/{login,signup} which return boot data. */
export default function AuthPage({ mode }: { mode: 'login' | 'signup' }) {
  const [login, setLogin] = useState('');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const signup = mode === 'signup';
  const { config } = getBoot();

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      if (signup) await session.signup({ login, email, password });
      else await session.login(login, password);
      const ret = new URLSearchParams(location.search).get('return_to');
      navigate(ret && ret.startsWith('/') && !ret.startsWith('//') ? ret : '/', { replace: true });
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Something went wrong. Try again.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.page}>
      <div className={styles.card}>
        <div className={styles.logo} aria-hidden>
          <svg viewBox="0 0 32 32" width="40" height="40">
            <rect width="32" height="32" rx="8" fill="var(--accent)" />
            <path d="M9 22.5V9.5h6.2c2.6 0 4.1 1.2 4.1 3.2 0 1.3-.7 2.3-1.9 2.7 1.6.3 2.6 1.5 2.6 3.1 0 2.4-1.8 4-4.6 4H9Zm3-7.6h2.7c1.2 0 1.9-.6 1.9-1.6s-.7-1.5-1.9-1.5H12v3.1Zm0 5.3h3c1.3 0 2-.6 2-1.7 0-1-.7-1.6-2-1.6h-3v3.3Z" fill="#fff" />
            <circle cx="23.5" cy="21" r="2.5" fill="#fff" />
          </svg>
        </div>
        <h1 className={styles.title}>{signup ? `Create your ${config.siteName} account` : `Sign in to ${config.siteName}`}</h1>
        {isMockMode() && <p className={styles.mock}>Mock mode — any credentials work.</p>}
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <Field label={signup ? 'Username' : 'Username or email address'} htmlFor="login">
            <Input id="login" size="lg" autoFocus autoComplete="username" value={login} onChange={(e) => setLogin(e.target.value)} required />
          </Field>
          {signup && (
            <Field label="Email" htmlFor="email">
              <Input id="email" size="lg" type="email" autoComplete="email" value={email} onChange={(e) => setEmail(e.target.value)} required />
            </Field>
          )}
          <Field label="Password" htmlFor="password" hint={signup ? 'At least 8 characters.' : undefined} error={error}>
            <Input
              id="password"
              size="lg"
              type="password"
              autoComplete={signup ? 'new-password' : 'current-password'}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              invalid={!!error}
              required
            />
          </Field>
          <Button type="submit" variant="primary" size="lg" block loading={busy}>
            {signup ? 'Create account' : 'Sign in'}
          </Button>
        </form>
        <p className={styles.switch}>
          {signup ? (
            <>
              Already have an account? <Link to="/login">Sign in</Link>
            </>
          ) : config.signupEnabled ? (
            <>
              New here? <Link to="/signup">Create an account</Link>
            </>
          ) : null}
        </p>
      </div>
    </div>
  );
}
