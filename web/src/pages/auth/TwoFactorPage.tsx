import { useState } from 'react';
import { returnTo } from '../../app/App';
import { session } from '../../app/session';
import { navigate, useLocation } from '../../router';
import { Button } from '../../ui/Button';
import { AlertIcon, ArrowLeftIcon, DeviceMobileIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, messageOf, StateBlock, statusOf } from './AuthPage';
import { TwoFactorForm, WrongCode } from './TwoFactorForm';

/**
 * `/login/two-factor?token=&return_to=`: second factor after an SSO sign-in
 * (sso.rs redirects accounts with 2FA here). Posts to
 * `/_bgh/session/two_factor`, then reloads boot data.
 */
export default function TwoFactorPage() {
  const { search } = useLocation();
  const token = new URLSearchParams(search).get('token') ?? '';
  const [expired, setExpired] = useState<string | null>(token ? null : 'This sign-in link is incomplete.');
  const loginHref = `/login${returnTo(search) === '/' ? '' : `?return_to=${encodeURIComponent(returnTo(search))}`}`;

  const verify = async (code: string) => {
    try {
      await session.completeTwoFactor(token, code);
      navigate(returnTo(search), { replace: true });
    } catch (e) {
      const status = statusOf(e);
      const msg = messageOf(e);
      if (status === 401 && /expired/i.test(msg)) return setExpired('Your sign-in expired. Please sign in again.');
      if (status === 429) return setExpired(msg);
      if (status === 401 || status === 422) throw new WrongCode('Incorrect two-factor code. Try again.', { cause: e });
      throw new Error(msg, { cause: e });
    }
  };

  if (expired) {
    return (
      <AuthLayout title="Two-factor authentication">
        <StateBlock
          icon={AlertIcon}
          tone="danger"
          title="Sign in again"
          actions={
            <Button size="lg" variant="primary" onClick={() => navigate(loginHref, { replace: true })}>
              Go to sign in
            </Button>
          }
        >
          {expired}
        </StateBlock>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout
      title="Two-factor authentication"
      subtitle="Your account is protected with two-factor authentication."
      hero={
        <span className={styles.twoFactorIcon}>
          <DeviceMobileIcon size={24} />
        </span>
      }
    >
      <TwoFactorForm
        verify={verify}
        footer={
          <div className={`${styles.small} ${styles.centered}`}>
            <button type="button" className={styles.linkButton} onClick={() => navigate(loginHref)}>
              <ArrowLeftIcon size={12} /> Back to sign in
            </button>
          </div>
        }
      />
    </AuthLayout>
  );
}
