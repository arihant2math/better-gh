import './ui/global.css';
import { configure } from 'mobx';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './app/App';
import { registerRoutes } from './app/routes';
import { dropShellCache, session } from './app/session';
import { bootIsStale, getBoot, isMockMode, setBoot, type BootData } from './boot';
import { shortcuts } from './shortcuts/manager';

configure({ enforceActions: 'never' });

async function main() {
  const mock = isMockMode();
  if (mock) {
    // The mock backend is its own chunk and never loads in normal use.
    const { installMock } = await import('./mock/index');
    await installMock();
  } else if (bootIsStale() || navigator.serviceWorker?.controller) {
    // Shell may have come from the SW cache (stale boot data, possibly another
    // session) or the dev server: refresh boot data in the background. Only
    // block when we have nothing at all (dev server).
    const refresh = fetch('/_bgh/boot', { credentials: 'same-origin', cache: 'no-store', headers: { Accept: 'application/json' } })
      .then((r) => (r.ok ? (r.json() as Promise<BootData>) : null))
      .catch(() => null);
    if (!getBoot().csrf) {
      const b = await refresh;
      if (b) setBoot(b);
    } else {
      void refresh.then((b) => {
        if (!b) return;
        const before = getBoot().user?.id;
        setBoot(b);
        if (b.user?.id !== before) {
          // The cached shell belongs to another session: drop it and reload from the network.
          dropShellCache();
          setTimeout(() => location.reload(), 50);
        }
      });
    }
  }

  registerRoutes();
  document.addEventListener('keydown', shortcuts.handleKeyDown);

  // Boot data is final now.
  session.init();
  if (session.user) void session.start();

  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );

  if (import.meta.env.PROD && 'serviceWorker' in navigator && !mock) {
    // Register after load so precaching never competes with first paint.
    const register = () => void navigator.serviceWorker.register('/sw.js').catch(() => undefined);
    if (document.readyState === 'complete') register();
    else window.addEventListener('load', register, { once: true });
  }
}

void main();
