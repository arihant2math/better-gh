import { useState } from 'react';
import { refresh, useResource } from '../../api/cache';
import { listAdminImports } from '../../api/metadataImports';
import { ErrorState, PageHeader, Panel } from '../../components/admin/kit';
import { navigate } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { DownloadIcon, PlusIcon, SyncIcon } from '../../ui/icons';
import { ImportForm } from '../imports/ImportForm';
import { ImportList } from '../imports/ImportList';

const KEY = 'admin:metadata-imports';

/** `/site-admin/imports`: GitHub / GHES metadata imports into any owner. */
export default function ImportsPage() {
  const [creating, setCreating] = useState(false);
  const list = useResource(KEY, listAdminImports);
  const reload = () => void refresh(KEY, listAdminImports).catch(() => undefined);
  return (
    <>
      <PageHeader
        title="Repository imports"
        description="Import repositories from GitHub.com or GitHub Enterprise Server with their issues, labels, milestones, releases and users."
        actions={
          <>
            <Button size="sm" leadingIcon={SyncIcon} onClick={reload}>
              Refresh
            </Button>
            <Button size="sm" variant="primary" leadingIcon={PlusIcon} onClick={() => setCreating((v) => !v)}>
              New import
            </Button>
          </>
        }
      />
      {creating && (
        <Panel title="New import">
          <ImportForm onCreated={(imp) => navigate(`/site-admin/imports/${imp.id}`)} />
        </Panel>
      )}
      {list.error ? (
        <ErrorState error={list.error} onRetry={reload} />
      ) : !list.data ? (
        <Skeleton height={120} />
      ) : list.data.length === 0 ? (
        !creating && (
          <EmptyState icon={DownloadIcon} title="No imports yet">
            Start one with “New import”, or run <code>bgh import github</code> on the server.
          </EmptyState>
        )
      ) : (
        <Panel padded={false} title={`${list.data.length} ${list.data.length === 1 ? 'import' : 'imports'}`}>
          <ImportList rows={list.data} detailPath={(id) => `/site-admin/imports/${id}`} />
        </Panel>
      )}
    </>
  );
}
