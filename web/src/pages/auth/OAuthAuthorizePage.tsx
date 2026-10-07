import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { getAuthorizeInfo, submitConsent, type AuthorizeInfo } from '../../api/auth';
import { session } from '../../app/session';
import { getBoot } from '../../boot';
import { useLocation } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Spinner } from '../../ui/Spinner';
import { AlertIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, Flash, messageOf, StateBlock, statusOf, useOnce } from './AuthPage';
import { AppHero, AppMeta, ScopeList } from './consent';

type State =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'consent'; info: AuthorizeInfo }
  | { kind: 'redirecting'; info: AuthorizeInfo | null; authorized: boolean };


/**
 * `/login/oauth/authorize?client_id&redirect_uri&scope&state&code_challenge…`:
 * OAuth web-flow consent. Apps already granted the requested scopes are
 * approved without a prompt (like github.com).
 */
export default observer(function OAuthAuthorizePage() {
  const { search } = useLocation();
  const [state, setState] = useState<State>({ kind: 'loading' });
  const [busy, setBusy] = useState<null | 'authorize' | 'cancel'>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const user = session.user;
  const siteName = getBoot().config.siteName;

  const finish = async (info: AuthorizeInfo, authorize: boolean) => {
    setBusy(authorize ? 'authorize' : 'cancel');
    setNotice(null);
    try {
      const { redirect_url } = await submitConsent(info.consent, authorize);
      setState({ kind: 'redirecting', info, authorized: authorize });
      location.assign(redirect_url);
    } catch (e) {
      // 404: the consent nonce expired (10 min) or was used in another tab.
      if (statusOf(e) === 404) setState({ kind: 'error', message: 'This authorization request expired. Start the sign-in from the application again.' });
      else setNotice(messageOf(e));
    } finally {
      setBusy(null);
    }
  };

  useOnce(search, () => {
    setState({ kind: 'loading' });
    getAuthorizeInfo(search).then(
      (info) => {
        if (info.already_authorized) {
          setState({ kind: 'redirecting', info, authorized: true });
          void finish(info, true);
        } else setState({ kind: 'consent', info });
      },
      (e: unknown) => setState({ kind: 'error', message: messageOf(e) }),
    );
  });

  if (state.kind === 'loading' || state.kind === 'redirecting') {
    const name = state.kind === 'redirecting' ? state.info?.app.name : null;
    return (
      <AuthLayout title={state.kind === 'loading' ? 'Authorize application' : state.authorized ? `Redirecting to ${name}` : 'Returning to the application'}>
        <StateBlock title={state.kind === 'loading' ? 'Loading…' : 'Redirecting…'}>
          <span className={styles.muted}>
            <Spinner size={20} />
          </span>
        </StateBlock>
      </AuthLayout>
    );
  }

  if (state.kind === 'error') {
    return (
      <AuthLayout title="Authorize application">
        <StateBlock
          icon={AlertIcon}
          tone="danger"
          title="Can't authorize this application"
          actions={
            <Button size="lg" variant="primary" onClick={() => (history.length > 1 ? history.back() : location.assign('/'))}>
              Go back
            </Button>
          }
        >
          {state.message}
          <div className={styles.small} style={{ marginTop: 8 }}>
            If you're the developer of this app, check its client ID and callback URL.
          </div>
        </StateBlock>
      </AuthLayout>
    );
  }

  const { info } = state;
  return (
    <AuthLayout
      wide
      title={`Authorize ${info.app.name}`}
      hero={<AppHero app={info.app} user={user} />}
      banner={
        notice && (
          <Flash tone="danger" onDismiss={() => setNotice(null)}>
            {notice}
          </Flash>
        )
      }
      below={<AppMeta app={info.app} redirectUri={info.redirect_uri} />}
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
          wants to access your <strong>@{user?.login}</strong> account on {siteName}
        </span>
      </div>
      <ScopeList scopes={info.scopes} />
      <div className={styles.consentActions}>
        <Button size="lg" disabled={!!busy} loading={busy === 'cancel'} onClick={() => void finish(info, false)}>
          Cancel
        </Button>
        <Button size="lg" variant="primary" disabled={!!busy} loading={busy === 'authorize'} onClick={() => void finish(info, true)}>
          Authorize {info.app.owner?.login ?? info.app.name}
        </Button>
      </div>
      <p className={`${styles.small} ${styles.muted} ${styles.centered}`} style={{ margin: '12px 0 0' }}>
        You can revoke access at any time in Settings → Applications.
      </p>
    </AuthLayout>
  );
});
