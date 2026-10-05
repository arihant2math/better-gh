import { useState } from 'react';
import { refresh, useResource } from '../../api/cache';
import { listAdminMirrors, syncMirror, type AdminMirror } from '../../api/imports';
import styles from '../../components/admin/admin.module.css';
import { ErrorState, PageHeader, Panel, StatusPill, attempt } from '../../components/admin/kit';
import { Link } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, SyncIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import m from './mirrors.module.css';

const key = (status: 'failed' | 'all') => `admin:mirrors:${status}`;

/** `/site-admin/mirrors`: pull mirrors, failing ones first. */
export default function MirrorsPage() {
  const [status, setStatus] = useState<'failed' | 'all'>('failed');
  const list = useResource(key(status), () => listAdminMirrors(status));
  const rows = list.data;
  const reload = () => void refresh(key(status), () => listAdminMirrors(status)).catch(() => undefined);

  return (
    <>
      <PageHeader
        title="Repository mirrors"
        description="Pull mirrors fetch from their upstream on a schedule. Mirrors whose last sync failed are listed first."
        actions={
          <Button size="sm" leadingIcon={SyncIcon} onClick={reload}>
            Refresh
          </Button>
        }
      />
      <Tabs
        size="sm"
        value={status}
        onChange={(v) => setStatus(v as 'failed' | 'all')}
        items={[
          { id: 'failed', label: 'Failing' },
          { id: 'all', label: 'All mirrors' },
        ]}
      />
      <div className={m.gap} />
      {list.error ? (
        <ErrorState error={list.error} onRetry={reload} />
      ) : !rows ? (
        <Skeleton height={120} />
      ) : rows.length === 0 ? (
        <EmptyState icon={CheckCircleIcon} title={status === 'failed' ? 'No failing mirrors' : 'No mirrors'}>
          {status === 'failed' ? 'Every mirror synced successfully on its last run.' : 'Create one from Import repository with “Mirror the repository”.'}
        </EmptyState>
      ) : (
        <Panel padded={false} title={`${rows.length} ${rows.length === 1 ? 'mirror' : 'mirrors'}`}>
          <ul className={m.list} aria-label="Mirrors">
            {rows.map((r) => (
              <MirrorRow key={r.repository} row={r} onSynced={reload} />
            ))}
          </ul>
        </Panel>
      )}
    </>
  );
}

function MirrorRow({ row, onSynced }: { row: AdminMirror; onSynced: () => void }) {
  const [owner, name] = row.repository.split('/') as [string, string];
  return (
    <li className={m.row}>
      <div className={m.main}>
        <div className={m.title}>
          <Link to={`/${row.repository}`}>{row.repository}</Link>
          <StatusPill status={row.last_status === 'failed' ? 'error' : row.last_status === 'success' ? 'ok' : 'neutral'}>
            {row.last_status === 'failed' ? `Failed${row.consecutive_failures > 1 ? ` ×${row.consecutive_failures}` : ''}` : row.last_status === 'success' ? 'Synced' : 'Pending'}
          </StatusPill>
          {!row.enabled && <StatusPill status="neutral">Paused</StatusPill>}
        </div>
        <div className={`${styles.mono} ${m.url}`}>{row.url}</div>
        {row.last_error && <div className={m.error}>{row.last_error}</div>}
      </div>
      <div className={m.meta}>
        {row.last_sync_at ? <RelativeTime date={row.last_sync_at} /> : 'never synced'}
        <Button size="sm" onClick={() => void attempt('Sync mirror', () => syncMirror(owner, name).then(onSynced), 'Sync queued')}>
          Sync now
        </Button>
      </div>
    </li>
  );
}
