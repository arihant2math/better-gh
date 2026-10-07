import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useResource } from '../../api/cache';
import { codeKeys, deleteRelease, findReleaseByTag, type RestRelease } from '../../api/code';
import { Link, navigate, prefetch as prefetchRoute, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, ChevronRightIcon, PencilIcon, TagIcon, TrashIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import styles from './Releases.module.css';
import { CardSkeletons, ReleaseCard, editHref, invalidateReleases, isNotFound, releasesBase, useCanPush, useLatestRelease, useTagShas } from './shared';

/** `/:owner/:repo/releases/tag/:tag` and `/:owner/:repo/releases/latest`. */
export default observer(function ReleasePage() {
  const { owner, repo, tag } = useParams<{ owner: string; repo: string; tag?: string }>();
  const byTag = useResource<RestRelease>(tag ? codeKeys.release(owner, repo, tag) : null, () => findReleaseByTag(owner, repo, tag!));
  const latest = useLatestRelease(owner, repo);
  const release = tag ? byTag.data : (latest.data ?? undefined);
  const canPush = useCanPush(owner, repo);
  const shas = useTagShas(owner, repo);
  const base = releasesBase(owner, repo);

  const crumbs = (
    <nav className={styles.crumbs} aria-label="Breadcrumbs">
      <Link to={base}>Releases</Link>
      <ChevronRightIcon size={14} />
      <strong>{release?.tag_name ?? tag ?? 'Latest'}</strong>
    </nav>
  );

  if (!release) {
    const error = tag ? byTag.error : latest.data === null ? 'none' : latest.error;
    const pending = tag ? !byTag.error : latest.data === undefined && !latest.error;
    return (
      <div className={styles.page}>
        {crumbs}
        {pending ? (
          <CardSkeletons count={1} />
        ) : (
          <EmptyState
            icon={error === 'none' || isNotFound(error) ? TagIcon : AlertIcon}
            title={error === 'none' ? 'There aren’t any releases here' : isNotFound(error) ? 'Release not found' : 'Could not load this release'}
            action={<Button onClick={() => navigate(base)}>View all releases</Button>}
          />
        )}
      </div>
    );
  }

  return (
    <div className={styles.page}>
      {crumbs}
      <Detail owner={owner} repo={repo} release={release} latestId={latest.data?.id ?? null} sha={shas.get(release.tag_name)} canPush={canPush} />
    </div>
  );
});

function Detail({
  owner,
  repo,
  release,
  latestId,
  sha,
  canPush,
}: {
  owner: string;
  repo: string;
  release: RestRelease;
  latestId: number | null;
  sha: string | undefined;
  canPush: boolean;
}) {
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const edit = editHref(owner, repo, release.tag_name);

  useShortcuts(
    'Release',
    {
      e: { handler: () => navigate(edit), description: 'Edit release', group: 'Releases' },
      'g r': { handler: () => navigate(releasesBase(owner, repo)), description: 'All releases', group: 'Releases' },
    },
    canPush,
  );

  const remove = async () => {
    setBusy(true);
    try {
      await deleteRelease(owner, repo, release.id);
      invalidateReleases(owner, repo);
      toast({ kind: 'success', title: `Deleted release ${release.name || release.tag_name}`, description: 'The tag was kept.' });
      navigate(releasesBase(owner, repo), { replace: true });
    } catch (e) {
      setBusy(false);
      toast({ kind: 'error', title: 'Could not delete the release', description: e instanceof Error ? e.message : String(e) });
    }
  };

  return (
    <>
      <ReleaseCard
        owner={owner}
        repo={repo}
        release={release}
        latest={release.id === latestId}
        sha={sha}
        linkTitle={false}
        assetsOpen
        actions={
          canPush ? (
            <>
              <Button size="sm" leadingIcon={PencilIcon} onClick={() => navigate(edit)} onMouseEnter={() => prefetchRoute(edit)}>
                Edit
              </Button>
              <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={() => setConfirm(true)}>
                Delete
              </Button>
            </>
          ) : undefined
        }
      />
      <Dialog
        open={confirm}
        onClose={() => !busy && setConfirm(false)}
        title="Delete this release?"
        footer={
          <>
            <Button onClick={() => setConfirm(false)} disabled={busy}>
              Cancel
            </Button>
            <Button variant="danger" loading={busy} onClick={() => void remove()}>
              Delete this release
            </Button>
          </>
        }
      >
        <p>
          This will permanently delete <strong>{release.name || release.tag_name}</strong> and its {release.assets.length} uploaded asset
          {release.assets.length === 1 ? '' : 's'}. The tag <code>{release.tag_name}</code> is kept.
        </p>
      </Dialog>
    </>
  );
}
