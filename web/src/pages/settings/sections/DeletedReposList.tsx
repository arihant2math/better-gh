import { useState } from 'react';
import { restoreRepo, type DeletedRepo } from '@/api/lifecycle';
import type { FullRepository } from '@/api/repoSettings';
import { ItemList, ItemRow, Pill, errorMessage } from '@/components/settings/kit';
import { Link } from '@/router';
import { Button } from '@/ui/Button';
import { LockIcon, RepoIcon, UndoIcon } from '@/ui/icons';
import { RelativeTime } from '@/ui/RelativeTime';
import { toast } from '@/ui/Toast';
import { Tooltip } from '@/ui/Tooltip';
import styles from './userSettings.module.css';

/** Why a row can't be restored (shown as the tooltip of the disabled button). */
export const NOT_RESTORABLE = 'The owner has a repository with this name again. Rename or delete it to restore this one.';

/**
 * Deleted repositories with Restore buttons, shared by
 * `/settings/repositories/deleted` and `/site-admin/repos/deleted`.
 */
export function DeletedReposList({ rows, onRestored, empty }: { rows: DeletedRepo[]; onRestored: (row: DeletedRepo, repo: FullRepository) => void; empty: string }) {
  return (
    <ItemList aria-label="Deleted repositories" empty={empty}>
      {rows.map((r) => (
        <DeletedRow key={r.id} row={r} onRestored={onRestored} />
      ))}
    </ItemList>
  );
}

function DeletedRow({ row, onRestored }: { row: DeletedRepo; onRestored: (row: DeletedRepo, repo: FullRepository) => void }) {
  const [busy, setBusy] = useState(false);
  const restore = async () => {
    setBusy(true);
    try {
      const repo = await restoreRepo(row.id);
      onRestored(row, repo);
      toast({ kind: 'success', title: `Restored ${repo.full_name ?? row.full_name}` });
    } catch (e) {
      toast({ kind: 'error', title: `Couldn’t restore ${row.full_name}`, description: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  const button = (
    <Button size="sm" leadingIcon={UndoIcon} loading={busy} disabled={!row.restorable} onClick={() => void restore()} aria-label={`Restore ${row.full_name}`}>
      Restore
    </Button>
  );
  return (
    <ItemRow
      icon={row.visibility === 'private' ? LockIcon : RepoIcon}
      title={
        <>
          <span className={styles.strong}>{row.full_name}</span>
          {row.visibility !== 'public' && <Pill>{row.visibility === 'internal' ? 'Internal' : 'Private'}</Pill>}
          {row.fork && <Pill>Fork</Pill>}
          {!row.restorable && <Pill tone="warning">Name taken</Pill>}
        </>
      }
      meta={
        <>
          Deleted <RelativeTime date={row.deleted_at} />
          {row.deleted_by && (
            <>
              {' '}
              by <Link to={`/${row.deleted_by.login}`}>{row.deleted_by.login}</Link>
            </>
          )}{' '}
          · will be permanently deleted <RelativeTime date={row.purge_at} />
        </>
      }
      actions={row.restorable ? button : <Tooltip label={NOT_RESTORABLE}><span tabIndex={0}>{button}</span></Tooltip>}
    />
  );
}
