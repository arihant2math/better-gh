import { observer } from 'mobx-react-lite';
import { useSyncExternalStore } from 'react';
import { formatKeys, shortcuts, type Binding } from '../shortcuts/manager';
import { Dialog } from '../ui/Dialog';
import styles from './ShortcutHelp.module.css';
import { ui } from './uiState';

let snapshot = shortcuts.list();
const subscribe = (fn: () => void) =>
  shortcuts.subscribe(() => {
    snapshot = shortcuts.list();
    fn();
  });

export const ShortcutHelp = observer(function ShortcutHelp() {
  const scopes = useSyncExternalStore(subscribe, () => snapshot);
  const groups = new Map<string, Binding[]>();
  const seen = new Set<string>();
  for (const s of scopes) {
    for (const b of s.bindings) {
      if (!b.description || seen.has(b.keys)) continue;
      seen.add(b.keys);
      const g = b.group ?? s.scope;
      groups.set(g, [...(groups.get(g) ?? []), b]);
    }
  }
  return (
    <Dialog open={ui.helpOpen} onClose={() => ui.setHelp(false)} title="Keyboard shortcuts" className={styles.dialog}>
      <div className={styles.grid}>
        {[...groups.entries()].map(([group, bindings]) => (
          <section key={group} className={styles.group}>
            <h3 className={styles.title}>{group}</h3>
            {bindings.map((b) => (
              <div key={b.keys} className={styles.row}>
                <span>{b.description}</span>
                <span className={styles.keys}>
                  {formatKeys(b.keys).map((k, i) => (
                    <span key={i} className={styles.chord}>
                      {i > 0 && <span className={styles.then}>then</span>}
                      <kbd>{k}</kbd>
                    </span>
                  ))}
                </span>
              </div>
            ))}
          </section>
        ))}
      </div>
    </Dialog>
  );
});

export default ShortcutHelp;
