import { useRef, useState } from 'react';
import { invalidate, useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { browseKeys, compareRefs, mergeUpstream } from '../../api/endpoints';
import type { RestCompare } from '../../api/types';
import { Link } from '../../router';
import { compareUrl } from '../../components/code/urls';
import { Button } from '../../ui/Button';
import { AlertIcon, ChevronDownIcon, GitPullRequestIcon, SyncIcon } from '../../ui/icons';
import { Popover } from '../../ui/Popover';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import { syncSummary } from './nav';
import styles from './RepoNav.module.css';

export const syncKey = (owner: string, name: string, upstreamOwner: string, branch: string) => `sync-fork:${owner}/${name}:${upstreamOwner}:${branch}`;

/**
 * "Sync fork" dropdown: ahead/behind of the fork's default branch against the
 * same branch upstream, and "Update branch" (`POST /merge-upstream`; a 409
 * means conflicts, which need a pull request instead).
 */
export function SyncFork({ owner, name, branch, upstream, canPush }: { owner: string; name: string; branch: string; upstream: { owner: string; name: string }; canPush: boolean }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [conflict, setConflict] = useState<string | null>(null);
  const anchor = useRef<HTMLButtonElement>(null);
  const key = syncKey(owner, name, upstream.owner, branch);
  const { data, error, loading } = useResource<RestCompare>(open ? key : null, () => compareRefs(owner, name, `${upstream.owner}:${branch}`, branch), { ttlMs: 15_000 });
  const upstreamLabel = `${upstream.owner}/${upstream.name}:${branch}`;

  const update = async () => {
    setBusy(true);
    setConflict(null);
    try {
      const res = await mergeUpstream(owner, name, branch);
      toast({ kind: 'success', title: res.merge_type === 'none' ? 'Branch is already up to date' : 'Fork synced', description: res.message });
      invalidate(key);
      invalidate(browseKeys.refs(owner, name));
      invalidate(`tree:${owner}/${name}@`);
      invalidate(`branches:${owner}/${name}`);
      setOpen(false);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) setConflict(e.message);
      else toast({ kind: 'error', title: 'Could not sync the fork', description: e instanceof Error ? e.message : undefined });
    } finally {
      setBusy(false);
    }
  };

  const behind = data?.behind_by ?? 0;
  const ahead = data?.ahead_by ?? 0;
  return (
    <>
      <Button ref={anchor} size="sm" leadingIcon={SyncIcon} trailingIcon={ChevronDownIcon} onClick={() => setOpen((v) => !v)} aria-expanded={open} aria-haspopup="dialog">
        Sync fork
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={anchor} placement="bottom-end" className={styles.syncPanel} role="dialog" aria-label="Sync fork">
        {error ? (
          <p className={styles.error}>{error instanceof ApiError && error.status === 404 ? `The upstream branch ${upstreamLabel} could not be compared.` : (error as Error).message}</p>
        ) : loading || !data ? (
          <p className={styles.muted}>
            <Spinner size={14} /> Comparing with {upstreamLabel}…
          </p>
        ) : conflict ? (
          <>
            <p className={styles.syncTitle}>
              <AlertIcon size={16} className={styles.warn} /> This branch has conflicts that must be resolved
            </p>
            <p className={styles.muted}>{conflict} Open a pull request to merge the upstream changes and resolve the conflicts.</p>
            <div className={styles.syncActions}>
              <Link to={compareUrl({ owner, repo: name }, branch, `${upstream.owner}:${branch}`, { expand: true })} className={styles.linkButton} onClick={() => setOpen(false)}>
                <GitPullRequestIcon size={16} /> Open pull request
              </Link>
            </div>
          </>
        ) : (
          <>
            <p className={styles.syncTitle}>{syncSummary(ahead, behind, upstreamLabel)}</p>
            <p className={styles.muted}>{behind > 0 ? 'Update your branch to keep it up to date with the upstream repository.' : 'No new commits to fetch. Enjoy your day!'}</p>
            <div className={styles.syncActions}>
              <Link to={compareUrl({ owner, repo: name }, `${upstream.owner}:${branch}`, branch)} className={styles.linkButton} onClick={() => setOpen(false)}>
                Compare
              </Link>
              {canPush && (
                <Button size="sm" variant="primary" disabled={behind === 0} loading={busy} onClick={() => void update()}>
                  Update branch
                </Button>
              )}
            </div>
          </>
        )}
      </Popover>
    </>
  );
}
