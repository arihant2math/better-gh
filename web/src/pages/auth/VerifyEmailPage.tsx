import { observer } from 'mobx-react-lite';
import { verifyEmail, type VerifiedEmail } from '../../api/auth';
import { session } from '../../app/session';
import { navigate, useLocation } from '../../router';
import { Button } from '../../ui/Button';
import { Spinner } from '../../ui/Spinner';
import { AlertIcon, CheckCircleIcon } from '../../ui/icons';
import { AuthLayout, authStyles as styles, messageOf, StateBlock, statusOf, useLoad } from './AuthPage';
import { resettableMap } from '../../api/reset';

/** Tokens are single use: one request per token even if the page remounts. */
const inflight = resettableMap<string, Promise<VerifiedEmail>>();

function verifyOnce(token: string): Promise<VerifiedEmail> {
  let p = inflight.get(token);
  if (!p) inflight.set(token, (p = verifyEmail(token)));
  return p;
}

/** `/settings/emails/verify?token=`: confirms an address from the mailed link. */
export default observer(function VerifyEmailPage() {
  const { search } = useLocation();
  const token = new URLSearchParams(search).get('token')?.trim() ?? '';
  const [state] = useLoad<VerifiedEmail>(token || null, () => verifyOnce(token));
  const signedIn = !!session.user;
  const next = signedIn ? (
    <Button size="lg" variant="primary" block onClick={() => navigate('/settings/emails')}>
      Go to email settings
    </Button>
  ) : (
    <Button size="lg" variant="primary" block onClick={() => navigate('/login?return_to=%2Fsettings%2Femails')}>
      Sign in
    </Button>
  );

  let body;
  if (!token) {
    body = (
      <StateBlock icon={AlertIcon} tone="danger" title="Missing verification token" actions={next}>
        Open the link from the verification email again, or request a new one from your email settings.
      </StateBlock>
    );
  } else if (state.status === 'loading') {
    body = (
      <StateBlock title="Verifying your email address…">
        <span className={styles.muted}>
          <Spinner size={20} />
        </span>
      </StateBlock>
    );
  } else if (state.status === 'ok') {
    body = (
      <StateBlock icon={CheckCircleIcon} tone="success" title="Email verified" actions={next}>
        <strong>{state.data.email}</strong> is now verified{signedIn ? '' : '. Sign in to manage your email addresses'}.
      </StateBlock>
    );
  } else {
    const s = statusOf(state.error);
    body = (
      <StateBlock icon={AlertIcon} tone="danger" title="This link is invalid or has expired" actions={next}>
        {s === 404 || s === 422
          ? 'Verification links can be used once and expire after a while. Request a new one from your email settings.'
          : messageOf(state.error)}
      </StateBlock>
    );
  }

  return <AuthLayout title="Verify your email">{body}</AuthLayout>;
});
