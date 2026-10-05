import { createRoot } from 'react-dom/client';
import SudoDialog from './SudoDialog';

let inflight: Promise<boolean> | null = null;

/**
 * Sudo mode prompt (lazy chunk): sensitive actions answer 401 "Sudo mode
 * required" when the session hasn't re-authenticated recently; the API
 * client calls this, which shows the dialog in its own root and resolves
 * true once sudo mode was granted. Concurrent requests share one prompt.
 */
export function promptSudo(): Promise<boolean> {
  inflight ??= new Promise<boolean>((resolve) => {
    const host = document.createElement('div');
    document.body.appendChild(host);
    const root = createRoot(host);
    const done = (ok: boolean) => {
      resolve(ok);
      // Unmount after the click that resolved us has finished.
      setTimeout(() => {
        root.unmount();
        host.remove();
      });
    };
    root.render(<SudoDialog onDone={done} />);
  }).finally(() => (inflight = null));
  return inflight;
}
