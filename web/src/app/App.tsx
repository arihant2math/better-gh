import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useEffect } from 'react';
import { navigate, RouterView, useLocation } from '../router';
import { Toaster } from '../ui/Toast';
import { NotFound } from './NotFound';
import { session } from './session';
import { Shell } from './Shell';

const AuthPage = lazy(() => import('../pages/auth/AuthPage'));

const AUTH_PATHS = new Set(['/login', '/signup']);

export const App = observer(function App() {
  const { pathname } = useLocation();
  const signedIn = !!session.user;
  const onAuthPage = AUTH_PATHS.has(pathname);

  useEffect(() => {
    if (!signedIn && !onAuthPage) navigate(`/login?return_to=${encodeURIComponent(pathname)}`, { replace: true });
    if (signedIn && onAuthPage) navigate('/', { replace: true });
    if (signedIn) void session.start();
  }, [signedIn, onAuthPage, pathname]);

  return (
    <>
      {!signedIn || onAuthPage ? (
        <Suspense fallback={null}>{onAuthPage && <AuthPage mode={pathname === '/signup' ? 'signup' : 'login'} />}</Suspense>
      ) : session.started ? (
        <Shell>
          <RouterView notFound={NotFound} />
        </Shell>
      ) : null}
      <Toaster />
    </>
  );
});
