import { observer } from 'mobx-react-lite';
import { useMemo } from 'react';
import { prefetch } from '../../api/cache';
import { codeKeys, listReleases, type RestRelease } from '../../api/code';
import { usePager } from '../../api/pager';
import { LoadMore } from '../../components/LoadMore';
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
  const latest = useLatestRelease(owner, repo).data ?? null;
  const shas = useTagShas(owner, repo);
  const newHref = `${releasesBase(owner, repo)}/new`;
  const spec = {
    key: (page: number) => codeKeys.releases(owner, repo, page),
    loader: (page: number) => listReleases(owner, repo, page, PER_PAGE),
    hasMore: (last: RestRelease[]) => last.length === PER_PAGE,
  };
  const pager = usePager<RestRelease[]>(`${owner}/${repo}`, spec);
  const { first, pages, hasMore } = pager;
  const releases = useMemo(() => pages.flat(), [pages]);
  const prefetchNext = () => prefetch(spec.key(pages.length + 1), () => spec.loader(pages.length + 1));

  useShortcuts(
    'Releases',
    { c: { handler: () => navigate(newHref), description: 'Draft a new release', group: 'Releases' } },
    canPush,
  );

  let body;
  if (!first.data) {
    body = first.error ? (
      <EmptyState icon={AlertIcon} title="Could not load releases" action={<Button onClick={pager.retry}>Retry</Button>}>
        {first.error instanceof Error ? first.error.message : null}
      </EmptyState>
    ) : (
      <CardSkeletons count={3} />
    );
  } else if (!releases.length) {
    body = (
      <EmptyState
        icon={TagIcon}
        title="There aren’t any releases here"
        action={
          canPush ? (
            <Button variant="primary" onClick={() => navigate(newHref)}>
              Create a new release
            </Button>
          ) : undefined
        }
      >
        Releases are powered by tagging specific points of history in a repository. They’re great for marking release points like v1.0.
      </EmptyState>
    );
  } else {
    body = (
      <>
        {releases.map((rel, i) => (
          <section key={rel.id} className={styles.row}>
            <ReleaseCard
              owner={owner}
              repo={repo}
              release={rel}
              latest={rel.id === latest?.id}
              sha={shas.get(rel.tag_name)}
              linkTitle
              assetsOpen={latest === null ? i === 0 : rel.id === latest.id}
              actions={
                canPush ? (
                  <IconButton icon={PencilIcon} label="Edit release" size="sm" onClick={() => navigate(editHref(owner, repo, rel.tag_name))} />
                ) : undefined
              }
            />
          </section>
        ))}
        {hasMore && <LoadMore pager={pager} className={styles.more} label="Load more" loadingLabel="Loading more releases…" onIntent={prefetchNext} />}
      </>
    );
  }

  return (
    <div className={styles.page}>
      <ReleasesHeader owner={owner} repo={repo} current="releases" canPush={canPush} />
      <div className={styles.list}>{body}</div>
    </div>
  );
});
