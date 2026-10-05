import { observer } from 'mobx-react-lite';
import { useEffect, useState } from 'react';
import { store } from '../../sync';
import { Button, cx } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Spinner } from '../../ui/Spinner';
import { loadWatchSettings, saveWatchSettings, watchStateOf, type WatchEvent, type WatchSettings, type WatchState } from './actions';
import styles from './WatchDialog.module.css';

const STATES: { id: WatchState; title: string; description: string }[] = [
  { id: 'participating', title: 'Participating and @mentions', description: 'Only receive notifications from this repository when participating or @mentioned.' },
  { id: 'all', title: 'All activity', description: 'Notified of all notifications on this repository.' },
  { id: 'ignore', title: 'Ignore', description: 'Never be notified.' },
  { id: 'custom', title: 'Custom', description: 'Select events you want to be notified of in addition to participating and @mentions.' },
];

const EVENTS: { id: WatchEvent; label: string }[] = [
  { id: 'issues', label: 'Issues' },
  { id: 'pulls', label: 'Pull requests' },
  { id: 'releases', label: 'Releases' },
  { id: 'discussions', label: 'Discussions' },
  { id: 'security_alerts', label: 'Security alerts' },
];

/** Per-repository watch settings (GitHub's Watch menu): participating / all / ignore / custom events. */
export default observer(function WatchDialog({ repoId, onClose }: { repoId: number; onClose: () => void }) {
  const repo = store().get('repo', repoId);
  const viewer = store().get('viewerRepo', repoId);
  const [settings, setSettings] = useState<WatchSettings | null>(null);
  const [draft, setDraft] = useState<WatchSettings>({ state: watchStateOf(viewer), events: [] });

  useEffect(() => {
    if (!repo) return;
    let live = true;
    void loadWatchSettings(repo).then((s) => {
      if (!live) return;
      setSettings(s);
      setDraft(s);
    });
    return () => {
      live = false;
    };
    // Load once per repository.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repoId]);

  if (!repo) return null;
  const invalid = draft.state === 'custom' && draft.events.length === 0;
  const changed = !settings || settings.state !== draft.state || (draft.state === 'custom' && draft.events.slice().sort().join() !== settings.events.slice().sort().join());
  const save = () => {
    saveWatchSettings(repo, draft);
    onClose();
  };
  const toggleEvent = (e: WatchEvent) =>
    setDraft((d) => ({ state: 'custom', events: d.events.includes(e) ? d.events.filter((x) => x !== e) : [...d.events, e] }));

  return (
    <Dialog
      open
      onClose={onClose}
      title={
        <>
          Watch <span className={styles.repo}>{repo.owner}/{repo.name}</span>
        </>
      }
      footer={
        <>
          {!settings && <Spinner size={14} />}
          <span style={{ flex: 1 }} />
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={invalid || !changed} onClick={save} kbd="↵">
            Apply
          </Button>
        </>
      }
    >
      <form
        className={styles.options}
        role="radiogroup"
        aria-label="Notification level"
        onSubmit={(e) => {
          e.preventDefault();
          if (!invalid && changed) save();
        }}
      >
        {STATES.map((s) => (
          <label key={s.id} className={cx(styles.option, draft.state === s.id && styles.selected)}>
            <input
              type="radio"
              name="watch"
              value={s.id}
              checked={draft.state === s.id}
              onChange={() => setDraft((d) => ({ state: s.id, events: s.id === 'custom' && d.events.length === 0 ? ['issues', 'pulls'] : d.events }))}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !invalid && changed) {
                  e.preventDefault();
                  save();
                }
              }}
            />
            <span>
              <span className={styles.title}>{s.title}</span>
              <span className={styles.description}>{s.description}</span>
              {s.id === 'custom' && draft.state === 'custom' && (
                <span className={styles.events}>
                  {EVENTS.map((e) => (
                    <label key={e.id} className={styles.event}>
                      <input type="checkbox" checked={draft.events.includes(e.id)} onChange={() => toggleEvent(e.id)} />
                      {e.label}
                    </label>
                  ))}
                  {invalid && <span className={styles.error}>Select at least one event.</span>}
                </span>
              )}
            </span>
          </label>
        ))}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
});
