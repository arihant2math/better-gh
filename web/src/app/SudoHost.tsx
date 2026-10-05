import { lazy, Suspense, useEffect, useState } from 'react';
import { setSudoHandler } from '../api/client';

const SudoDialog = lazy(() => import('./SudoDialog'));

/**
 * Sudo mode prompt: sensitive actions answer 401 "Sudo mode required" when
 * the session hasn't re-authenticated recently; the API client asks this
 * host, which shows the (lazy) dialog and retries the request on success.
 * Concurrent requests share one prompt.
 */
export function SudoHost() {
  const [resolve, setResolve] = useState<((ok: boolean) => void) | null>(null);

  useEffect(() => {
    let inflight: Promise<boolean> | null = null;
    return setSudoHandler(() => {
      inflight ??= new Promise<boolean>((done) => setResolve(() => done)).finally(() => (inflight = null));
      return inflight;
    });
  }, []);

  if (!resolve) return null;
  const finish = (ok: boolean) => {
    resolve(ok);
    setResolve(null);
  };
  return (
    <Suspense fallback={null}>
      <SudoDialog onDone={finish} />
    </Suspense>
  );
}
