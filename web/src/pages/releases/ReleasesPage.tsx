import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { prefetch, useResource } from '../../api/cache';
import { codeKeys, listReleases, type RestRelease } from '../../api/code';
import { navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, PencilIcon, TagIcon } from '../../ui/icons';
import styles from './Releases.module.css';
import { CardSkeletons, PER_PAGE, ReleaseCard, ReleasesHeader, editHref, releasesBase, useCanPush, useLatestRelease, useTagShas } from './shared';

/** `/:owner/:repo/releases` — newest first, paginated with "Load more". */
export default observer(function ReleasesPage() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>();
  const canPush = useCanPush(owner, repo);
  const [pages, setPages] = useState(1);
  const latest = useLatestRelease(owner, repo).data ?? null;
  const shas = useTagShas(owner, repo);
  const newHref = `${releasesBase(owner, repo)}/new`;

  useShortcuts(
    'Releases',
    { c: { handler: () => navigate(newHref), description: 'Draft a new release', group: 'Releases' } },
    canPush,
  );

  return (
    <div className={styles.page}>
      <ReleasesHeader owner={owner} repo={repo} current="releases" canPush={canPush} />
      <div className={styles.list}>
        {Array.from({ length: pages }, (_, i) => (
          <Chunk
            key={i}
            owner={owner}
            repo={repo}
            page={i + 1}
            last={i + 1 === pages}
            onMore={() => setPages(pages + 1)}
            latestId={latest?.id ?? null}
            shas={shas}
            canPush={canPush}
          />
        ))}
      </div>
    </div>
  );
});

function Chunk({
  owner,
  repo,
  page,
  last,
  onMore,
  latestId,
  shas,
  canPush,
}: {
  owner: string;
  repo: string;
  page: number;
  last: boolean;
  onMore: () => void;
  latestId: number | null;
  shas: Map<string, string>;
  canPush: boolean;
}) {
  const res = useResource<RestRelease[]>(codeKeys.releases(owner, repo, page), () => listReleases(owner, repo, page, PER_PAGE));
  const loadNext = () => prefetch(codeKeys.releases(owner, repo, page + 1), () => listReleases(owner, repo, page + 1, PER_PAGE));
  if (!res.data) {
    if (res.error) {
      return (
        <EmptyState icon={AlertIcon} title="Could not load releases" action={<Button onClick={() => location.reload()}>Retry</Button>}>
          {res.error instanceof Error ? res.error.message : null}
        </EmptyState>
      );
    }
    return <CardSkeletons count={page === 1 ? 3 : 1} />;
  }
  if (page === 1 && !res.data.length) {
    return (
      <EmptyState
        icon={TagIcon}
        title="There aren’t any releases here"
        action={
          canPush ? (
            <Button variant="primary" onClick={() => navigate(`${releasesBase(owner, repo)}/new`)}>
              Create a new release
            </Button>
          ) : undefined
        }
      >
        Releases are powered by tagging specific points of history in a repository. They’re great for marking release points like v1.0.
      </EmptyState>
    );
  }
  return (
    <>
      {res.data.map((rel, i) => (
        <section key={rel.id} className={styles.row}>
          <ReleaseCard
            owner={owner}
            repo={repo}
            release={rel}
            latest={rel.id === latestId}
            sha={shas.get(rel.tag_name)}
            linkTitle
            assetsOpen={latestId === null ? page === 1 && i === 0 : rel.id === latestId}
            actions={
              canPush ? (
                <IconButton icon={PencilIcon} label="Edit release" size="sm" onClick={() => navigate(editHref(owner, repo, rel.tag_name))} />
              ) : undefined
            }
          />
        </section>
      ))}
      {last && res.data.length === PER_PAGE && (
        <div className={styles.more}>
          <Button onClick={onMore} onMouseEnter={loadNext} onFocus={loadNext}>
            Load more
          </Button>
        </div>
      )}
    </>
  );
}
