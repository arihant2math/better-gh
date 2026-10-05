import { useState } from 'react';
import { mutate, refresh, useResource } from '../../api/cache';
import { lifecycleKeys, listAdminDeletedRepos, type DeletedRepo } from '../../api/lifecycle';
import { ErrorState, PageHeader, Panel, SearchInput } from '../../components/admin/kit';
import { DeletedReposList } from '../settings/sections/DeletedReposList';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { SyncIcon } from '../../ui/icons';

const key = lifecycleKeys.adminDeleted();

/** `/site-admin/repos/deleted`: every repository deleted in the last 90 days, restorable by site admins. */
export default function DeletedReposPage() {
  const list = useResource(key, listAdminDeletedRepos);
  const [q, setQ] = useState('');
  const reload = () => void refresh(key, listAdminDeletedRepos).catch(() => undefined);
  const needle = q.trim().toLowerCase();
  const rows = list.data?.filter((r) => !needle || r.full_name.toLowerCase().includes(needle) || r.deleted_by?.login.toLowerCase().includes(needle));
  return (
    <>
      <PageHeader
        title="Deleted repositories"
        description="Repositories deleted in the last 90 days on this instance. They are purged permanently after that; until then an owner or a site administrator can restore them."
        actions={
          <Button size="sm" leadingIcon={SyncIcon} onClick={reload}>
            Refresh
          </Button>
        }
      />
      {list.error && !list.data ? (
        <ErrorState error={list.error} onRetry={reload} />
      ) : !rows ? (
        <Skeleton height={120} />
      ) : (
        <Panel
          padded={false}
          title={`${rows.length} deleted ${rows.length === 1 ? 'repository' : 'repositories'}`}
          actions={<SearchInput value={q} onChange={setQ} label="Filter deleted repositories" placeholder="Filter by name or user" width={240} />}
        >
          <DeletedReposList
            rows={rows}
            empty={needle ? 'No deleted repositories match this filter.' : 'No repositories were deleted in the last 90 days.'}
            onRestored={(row) => mutate<DeletedRepo[]>(key, (prev) => (prev ?? []).filter((r) => r.id !== row.id))}
          />
        </Panel>
      )}
    </>
  );
}
