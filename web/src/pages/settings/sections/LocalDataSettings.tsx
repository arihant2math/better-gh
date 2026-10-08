import { observer } from 'mobx-react-lite';
import { PageHeader } from '@/components/settings/kit';
import { MODEL_NAMES } from '@/sync/schema';
import { store, sync } from '@/sync';
import { Button } from '@/ui/Button';
import styles from '../SettingsLayout.module.css';

export default observer(function LocalDataSettings() {
  const c = sync();
  const s = store();
  return (
    <>
      <PageHeader
        title="Local data & sync"
        description="Everything you see is served from a local database kept in sync over a WebSocket. Mutations apply instantly and are sent in the background."
      />
      <div className={styles.form}>
        <dl className={styles.stats}>
          <dt>Status</dt>
          <dd>{c.status}</dd>
          <dt>Last sync id</dt>
          <dd>{c.lastSyncId}</dd>
          <dt>Pending changes</dt>
          <dd>{c.queue.pendingCount}</dd>
          <dt>Subscribed scopes</dt>
          <dd>{c.scopes.size}</dd>
          {MODEL_NAMES.map((m) => (
            <span key={m} style={{ display: 'contents' }}>
              <dt>{m}</dt>
              <dd>{s.count(m).toLocaleString()}</dd>
            </span>
          ))}
        </dl>
        <div>
          <Button
            variant="danger"
            onClick={() => void c.bootstrap()}
          >
            Re-download workspace
          </Button>
        </div>
      </div>
    </>
  );
});
