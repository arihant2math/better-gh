import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { decideDevice, formatUserCode, getDeviceRequest, isCompleteUserCode, type DeviceInfo } from '../../api/auth';
import { session } from '../../app/session';
import { loginHref, navigate, useLocation } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { CheckCircleIcon, CircleSlashIcon, DeviceDesktopIcon, TerminalIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, Flash, messageOf, StateBlock, statusOf, useOnce } from './AuthPage';
import { AppHero, AppMeta, ScopeList } from './consent';

type Step = { kind: 'enter' } | { kind: 'confirm'; info: DeviceInfo } | { kind: 'done'; authorized: boolean; info: DeviceInfo };

const INVALID = 'The code you entered is invalid or has expired. Check the code on your device and try again.';

function initialCode(pathname: string, search: string): string {
  const fromPath = pathname.match(/^\/login\/device\/([^/]+)/)?.[1];
  return formatUserCode(decodeURIComponent(fromPath ?? new URLSearchParams(search).get('user_code') ?? ''));
}

/**
 * `/login/device`: device flow activation (`gh auth login --web`). Enter the
 * code shown in the terminal, review the app, authorize or cancel.
 */
export default function DevicePage() {
  const { pathname, search } = useLocation();
  return <Device key={pathname + search} pathname={pathname} search={search} />;
}

const Device = observer(function Device({ pathname, search }: { pathname: string; search: string }) {
  const [code, setCode] = useState(() => initialCode(pathname, search));
  const [step, setStep] = useState<Step>({ kind: 'enter' });
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<null | 'lookup' | 'authorize' | 'cancel'>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const user = session.user;

  const lookup = async (value = code) => {
    if (!isCompleteUserCode(value)) {
      setError('Enter the 8-character code shown on your device, like WDJB-MJHT.');
      inputRef.current?.focus();
      return;
    }
    setBusy('lookup');
    setError(null);
    setNotice(null);
    try {
      setStep({ kind: 'confirm', info: await getDeviceRequest(value) });
    } catch (e) {
      if (statusOf(e) === 404) setError(INVALID);
      else setNotice(messageOf(e));
      requestAnimationFrame(() => inputRef.current?.select());
    } finally {
      setBusy(null);
    }
  };

  // A prefilled code (from the terminal's link) goes straight to the review.
  const pre = initialCode(pathname, search);
  useOnce(isCompleteUserCode(pre) ? pre : null, () => void lookup(pre));

  const decide = async (info: DeviceInfo, authorize: boolean) => {
    setBusy(authorize ? 'authorize' : 'cancel');
    setNotice(null);
    try {
      await decideDevice(info.user_code, authorize);
      setStep({ kind: 'done', authorized: authorize, info });
    } catch (e) {
      if (statusOf(e) === 404) {
        setStep({ kind: 'enter' });
        setError(INVALID);
      } else setNotice(messageOf(e));
    } finally {
      setBusy(null);
    }
  };

  const banner = notice && (
    <Flash tone="danger" onDismiss={() => setNotice(null)}>
      {notice}
    </Flash>
  );

  if (step.kind === 'done') {
    return (
      <AuthLayout title={step.authorized ? 'Device connected' : 'Authorization cancelled'}>
        {step.authorized ? (
          <StateBlock icon={CheckCircleIcon} tone="success" title="Congratulations, you're all set!">
            <strong>{step.info.app.name}</strong> can now access your account. You can close this window and return to your terminal.
          </StateBlock>
        ) : (
          <StateBlock
            icon={CircleSlashIcon}
            title="The device was not connected"
            actions={
              <Button
                onClick={() => {
                  setCode('');
                  setStep({ kind: 'enter' });
                }}
              >
                Enter another code
              </Button>
            }
          >
            You can close this window. Run the sign-in command again to start over.
          </StateBlock>
        )}
      </AuthLayout>
    );
  }

  if (step.kind === 'confirm') {
    const { info } = step;
    return (
      <AuthLayout
        wide
        title={`Authorize ${info.app.name}`}
        hero={<AppHero app={info.app} user={user} />}
        banner={banner}
        below={<AppMeta app={info.app} extra={<>Device code <span className={styles.mono}>{info.user_code}</span></>} />}
      >
        <div className={styles.who}>
          <Avatar user={user} size={32} />
          <span>
            <strong>{info.app.name}</strong>
            {info.app.owner ? (
              <>
                {' '}
                by <strong>@{info.app.owner.login}</strong>
              </>
            ) : null}{' '}
            wants to access your <strong>@{user?.login}</strong> account
          </span>
        </div>
        <ScopeList scopes={info.scopes} />
        <div className={styles.consentActions}>
          <Button size="lg" disabled={!!busy} loading={busy === 'cancel'} onClick={() => void decide(info, false)}>
            Cancel
          </Button>
          <Button size="lg" variant="primary" disabled={!!busy} loading={busy === 'authorize'} onClick={() => void decide(info, true)}>
            Authorize
          </Button>
        </div>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout
      title="Device activation"
      subtitle="Enter the code displayed on your device"
      hero={
        <span className={styles.twoFactorIcon}>
          <TerminalIcon size={24} />
        </span>
      }
      banner={banner}
      below={
        <span className={styles.small}>
          Signed in as <strong>@{user?.login}</strong>. Not you?{' '}
          <button type="button" className={styles.linkButton} onClick={() => void session.logout().then(() => navigate(loginHref(pathname + search)))}>
            Sign out
          </button>
        </span>
      }
    >
      <form
        className={styles.form}
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void lookup();
        }}
      >
        <div className={styles.fieldGroup}>
          <label htmlFor="user_code" className={styles.labelRow}>
            <span>
              <DeviceDesktopIcon size={14} /> Device code
            </span>
          </label>
          <input
            ref={inputRef}
            id="user_code"
            className={`${styles.code} ${error ? styles.otpInvalid : ''}`}
            value={code}
            placeholder="XXXX-XXXX"
            autoFocus
            autoComplete="off"
            autoCapitalize="characters"
            spellCheck={false}
            aria-invalid={!!error || undefined}
            aria-describedby="code-help"
            onChange={(e) => {
              setCode(formatUserCode(e.target.value));
              setError(null);
            }}
            onPaste={(e) => {
              const text = e.clipboardData.getData('text');
              const next = formatUserCode(text);
              if (!next) return;
              e.preventDefault();
              setCode(next);
              setError(null);
              if (isCompleteUserCode(next)) void lookup(next);
            }}
          />
          {error ? (
            <div id="code-help" className={styles.fieldError} role="alert">
              {error}
            </div>
          ) : (
            <div id="code-help" className={styles.fieldHint}>
              It's shown in your terminal or on the device, for example after <span className={styles.mono}>gh auth login --web</span>.
            </div>
          )}
        </div>
        <Button type="submit" variant="primary" size="lg" block loading={busy === 'lookup'} disabled={!code}>
          Continue
        </Button>
      </form>
    </AuthLayout>
  );
});
