import { useEffect, type ReactNode } from 'react';
import { isSha } from '@/api/endpoints';
import { useRefs } from '@/components/code/RefPicker';
import { useParams } from '@/router';
import { canPush } from '@/sync/selectors';
import { cx } from '@/ui/Button';
import { AlertIcon, InfoIcon, LockIcon } from '@/ui/icons';
import { useRouteRepo } from '../../repo/useRouteRepo';
import { splitRefPath } from '../util';
import styles from './Edit.module.css';

/** Route params + repo, resolved ref and push permission for the edit pages. */
export function useEditTarget() {
  const params = useParams<{ owner: string; repo: string; ref: string; '*'?: string }>();
  const owner = params.owner;
  const name = params.repo;
  const repo = useRouteRepo();
  const refs = useRefs(owner, name);
  // Same split as the code view (longest branch/tag prefix wins).
  const refParam = params.ref ?? repo.defaultBranch;
  const rest = (params['*'] ?? '').replace(/^\/+|\/+$/g, '');
  const { ref, path } = (refs.data && splitRefPath(refs.data, refParam, rest)) || { ref: refParam, path: rest };
  const isBranch = refs.data ? refs.data.branches.some((b) => b.name === ref) : !isSha(ref);
  const isTag = !!refs.data?.tags.some((t) => t.name === ref);
  return { owner, name, repo, ref, path, refs, isBranch, isTag, canPush: canPush(repo.id) };
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
