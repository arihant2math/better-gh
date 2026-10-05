import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useEffect, type ComponentType, type LazyExoticComponent } from 'react';
import { navigate, RouterView, useLocation } from '../router';
import { Toaster } from '../ui/Toast';
import { NotFound } from './NotFound';
import { session } from './session';
import { Shell } from './Shell';

/**
 * Full-page screens rendered without the app shell (sign-in flows, OAuth
 * consent, device activation). `public` pages work signed out; the others
 * send signed-out visitors to /login with a `return_to`.
 */
interface BarePage {
  re: RegExp;
  page: LazyExoticComponent<ComponentType>;
  public: boolean;
  /** Signed-in visitors are sent onwards (to `return_to` or `/`). */
  guestOnly?: boolean;
}

const BARE: BarePage[] = [
  { re: /^\/login\/?$/, page: lazy(() => import('../pages/auth/LoginPage')), public: true, guestOnly: true },
  { re: /^\/signup\/?$/, page: lazy(() => import('../pages/auth/SignupPage')), public: true, guestOnly: true },
  { re: /^\/login\/two-factor\/?$/, page: lazy(() => import('../pages/auth/TwoFactorPage')), public: true, guestOnly: true },
  { re: /^\/password_reset(\/[^/]+)?\/?$/, page: lazy(() => import('../pages/auth/PasswordResetPage')), public: true },
  { re: /^\/settings\/emails\/verify\/?$/, page: lazy(() => import('../pages/auth/VerifyEmailPage')), public: true },
  { re: /^\/login\/device(\/[^/]*)?\/?$/, page: lazy(() => import('../pages/auth/DevicePage')), public: false },
  { re: /^\/login\/oauth\/authorize\/?$/, page: lazy(() => import('../pages/auth/OAuthAuthorizePage')), public: false },
];

function bareFor(pathname: string): BarePage | undefined {
  return BARE.find((b) => b.re.test(pathname));
}

/** Where to go after signing in: a same-origin `return_to`, else home. */
export function returnTo(search = location.search): string {
  const ret = new URLSearchParams(search).get('return_to');
  return ret && ret.startsWith('/') && !ret.startsWith('//') ? ret : '/';
}

export const App = observer(function App() {
  const { pathname, search } = useLocation();
  const signedIn = !!session.user;
  const bare = bareFor(pathname);

  useEffect(() => {
    if (!signedIn && !bare?.public) navigate(`/login?return_to=${encodeURIComponent(pathname + search)}`, { replace: true });
    if (signedIn && bare?.guestOnly) navigate(returnTo(search), { replace: true });
    if (signedIn) void session.start();
  }, [signedIn, bare, pathname, search]);

  let content = null;
  if (bare) {
    const Page = bare.page;
    if (bare.public || signedIn) content = <Suspense fallback={null}>{!(signedIn && bare.guestOnly) && <Page />}</Suspense>;
  } else if (signedIn && session.started) {
    content = (
      <Shell>
        <RouterView notFound={NotFound} />
      </Shell>
    );
  }

  return (
    <>
      {content}
      <Toaster />
    </>
  );
});
