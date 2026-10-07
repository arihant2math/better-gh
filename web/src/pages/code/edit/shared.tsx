import { useEffect, type ReactNode } from 'react';
import type { BrowseRefs } from '../../../api/types';
import { isSha } from '../../../api/endpoints';
import { useRefs } from '../../../components/code/RefPicker';
import { useParams } from '../../../router';
import { store } from '../../../sync';
import { repoByName } from '../../../sync/selectors';
import { cx } from '../../../ui/Button';
import { AlertIcon, InfoIcon, LockIcon } from '../../../ui/icons';
import styles from './Edit.module.css';

/**
 * Split `{ref}/{rest}` when the ref itself contains slashes
 * (`/edit/feature/x/src/a.ts` → ref `feature/x`, path `src/a.ts`).
 */
export function resolveRefPath(ref: string, rest: string, refs: BrowseRefs | undefined): { ref: string; path: string } {
  const path = rest.replace(/^\/+|\/+$/g, '');
  if (!refs || !path) return { ref, path };
  const full = `${ref}/${path}`;
  let best = '';
  for (const r of [...refs.branches, ...refs.tags]) {
    if (r.name.length > best.length && r.name.startsWith(`${ref}/`) && (full === r.name || full.startsWith(`${r.name}/`))) best = r.name;
  }
  return best ? { ref: best, path: full.slice(best.length + 1) } : { ref, path };
}

/** Route params + repo, resolved ref and push permission for the edit pages. */
export function useEditTarget() {
  const params = useParams<{ owner: string; repo: string; ref: string; '*'?: string }>();
  const owner = params.owner;
  const name = params.repo;
  const repo = repoByName(owner, name);
  const refs = useRefs(owner, name);
  const { ref, path } = resolveRefPath(params.ref ?? repo?.defaultBranch ?? '', params['*'] ?? '', refs.data);
  const isBranch = refs.data ? refs.data.branches.some((b) => b.name === ref) : !isSha(ref);
  const isTag = !!refs.data?.tags.some((t) => t.name === ref);
  const permission = repo ? store().get('viewerRepo', repo.id)?.permission : undefined;
  const canPush = permission === 'admin' || permission === 'maintain' || permission === 'write';
  return { owner, name, repo, ref, path, refs, isBranch, isTag, canPush, permission };
}

export function directBlockedReason(t: { ref: string; isBranch: boolean; isTag: boolean }): string | undefined {
  if (t.isBranch) return undefined;
  return t.isTag ? `${t.ref} is a tag; commits can only go to branches.` : `You’re viewing a commit, not a branch.`;
}

export function Notice({ kind = 'info', children, actions }: { kind?: 'info' | 'warning' | 'danger'; children: ReactNode; actions?: ReactNode }) {
  const I = kind === 'info' ? InfoIcon : kind === 'warning' ? LockIcon : AlertIcon;
  return (
    <div className={cx(styles.notice, styles[`notice_${kind}`])} role={kind === 'danger' ? 'alert' : 'status'}>
      <I size={16} className={styles.noticeIcon} />
      <div className={styles.noticeBody}>{children}</div>
      {actions && <div className={styles.noticeActions}>{actions}</div>}
    </div>
  );
}

export function NoPushNotice({ repo }: { repo: string }) {
  return (
    <Notice kind="warning">
      You don’t have write access to <strong>{repo}</strong>. Committing is disabled — proposing changes from a fork isn’t supported here yet.
    </Notice>
  );
}

/** Warn before closing / reloading the tab while `dirty`. */
export function useUnloadGuard(dirty: boolean): void {
  useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onBeforeUnload);
    return () => window.removeEventListener('beforeunload', onBeforeUnload);
  }, [dirty]);
}
