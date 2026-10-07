import { observer } from 'mobx-react-lite';
import { useEffect, useMemo, useRef, useState } from 'react';
import { peek, useResource } from '../../api/cache';
import { codeKeys, listReleases, listTags, type RestRelease, type RestTag } from '../../api/code';
import { getHistory } from '../../api/endpoints';
import { usePager } from '../../api/pager';
import type { History } from '../../api/types';
import { LoadMore } from '../../components/LoadMore';
import { Link, useParams } from '../../router';
import type { Repo } from '../../sync/models';
import { repoByName } from '../../sync/selectors';
import { Button, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, FileZipIcon, GitCommitIcon, RocketIcon, TagIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { TabNav } from '../../ui/Tabs';
import { VirtualList } from '../../ui/VirtualList';
import styles from './Branches.module.css';

const enc = encodeURIComponent;
const PER_PAGE = 100;
const VIRTUALIZE_OVER = 100;

/** `/:owner/:repo/tags`. */
export default observer(function TagsPage() {
  const params = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(params.owner, params.repo);
  if (!repo) return null;
  return <Tags repo={repo} />;
});

function tagsKey(repo: Repo, page: number) {
  // Page 1 shares the route-prefetch key.
  return page === 1 ? codeKeys.tags(repo.owner, repo.name) : `${codeKeys.tags(repo.owner, repo.name)}#${page}`;
}

function Tags({ repo }: { repo: Repo }) {
  const o = repo.owner;
  const r = repo.name;
  const base = `/${o}/${r}`;
  const pager = usePager<RestTag[]>(`${o}/${r}`, {
    key: (page) => tagsKey(repo, page),
    // Page 1 without paging params: it shares the route prefetch's request.
    loader: (page) => (page === 1 ? listTags(o, r) : listTags(o, r, page, PER_PAGE)),
    hasMore: (last) => last.length >= PER_PAGE,
  });
  const { first, pages, hasMore } = pager;
  const releases = useResource<RestRelease[]>(codeKeys.releaseTags(o, r), () => listReleases(o, r, 1, 100));

  const tags = useMemo(() => pages.flat(), [pages]);
  const releaseByTag = useMemo(() => new Map((releases.data ?? []).filter((x) => !x.draft).map((x) => [x.tag_name, x])), [releases.data]);

  let body;
  if (first.error && !first.data) body = <EmptyState icon={AlertIcon} title="Couldn’t load tags" action={<Button onClick={pager.retry}>Retry</Button>} />;
  else if (!first.data) body = <SkeletonTags />;
  else if (!tags.length)
    body = (
      <EmptyState icon={TagIcon} title="There aren’t any tags yet">
        Tags mark specific points in the history, usually releases.
      </EmptyState>
    );
  else
    body = (
      <section className={styles.section}>
        <div className={styles.box} role="list" aria-label="Tags">
          {tags.map((t) => (
            <TagRow key={t.name} repo={repo} tag={t} release={releaseByTag.get(t.name)} />
          ))}
        </div>
        {hasMore && <LoadMore pager={pager} label="Load more tags" style={{ marginTop: 'var(--sp-3)', textAlign: 'center' }} />}
      </section>
    );

  return (
    <div className={styles.page}>
      <div className={styles.toolbar}>
        <TabNav
          aria-label="Releases and tags"
          current="tags"
          items={[
            { id: 'releases', label: 'Releases', icon: RocketIcon, href: `${base}/releases` },
            { id: 'tags', label: 'Tags', icon: TagIcon, href: `${base}/tags`, count: first.data ? `${tags.length}${hasMore ? '+' : ''}` : undefined },
          ]}
        />
      </div>
      {tags.length > VIRTUALIZE_OVER ? (
        <VirtualList
          className={styles.virtual}
          items={tags}
          estimateSize={64}
          aria-label="Tags"
          getKey={(t) => t.name}
          renderItem={(t, i) => (
            <div className={cx(styles.vrow, i === 0 && styles.vfirst, i === tags.length - 1 && styles.vlast)} style={i === 0 ? { marginTop: 'var(--sp-4)' } : undefined}>
              <TagRow repo={repo} tag={t} release={releaseByTag.get(t.name)} />
              {i === tags.length - 1 && hasMore && <LoadMore pager={pager} auto label="Load more tags" style={{ padding: 'var(--sp-3)', textAlign: 'center' }} />}
            </div>
          )}
        />
      ) : (
        <div className={styles.scroll}>{body}</div>
      )}
    </div>
  );
}

function TagRow({ repo, tag, release }: { repo: Repo; tag: RestTag; release: RestRelease | undefined }) {
  const base = `/${repo.owner}/${repo.name}`;
  const archive = `${base}/archive/refs/tags/${enc(tag.name)}`;
  return (
    <div className={styles.tagRow} role="listitem">
      <div>
        <Link to={`${base}/tree/${enc(tag.name)}`} className={styles.tagName}>
          <TagIcon size={16} />
          {tag.name}
        </Link>
        <div className={styles.tagMeta}>
          <TagDate repo={repo} sha={tag.commit.sha} />
          <Link to={`${base}/commit/${tag.commit.sha}`} className={styles.mono} title={tag.commit.sha}>
            <GitCommitIcon size={14} />
            {tag.commit.sha.slice(0, 7)}
          </Link>
          <a href={`${archive}.zip`} download>
            <FileZipIcon size={14} />
            zip
          </a>
          <a href={`${archive}.tar.gz`} download>
            <FileZipIcon size={14} />
            tar.gz
          </a>
        </div>
      </div>
      {release && (
        <Link to={`${base}/releases/tag/${enc(tag.name)}`} className={styles.newPr}>
          <RocketIcon size={14} />
          {release.prerelease ? 'Pre-release' : 'Release'}
          {release.name && release.name !== tag.name ? ` · ${release.name}` : ''}
        </Link>
      )}
    </div>
  );
}

/** Date of the tagged commit, fetched only once the row scrolls into view (immutable per SHA). */
function TagDate({ repo, sha }: { repo: Repo; sha: string }) {
  const ref = useRef<HTMLSpanElement>(null);
  const key = codeKeys.commitBrief(repo.owner, repo.name, sha);
  const [visible, setVisible] = useState(() => peek(key) !== undefined);
  useEffect(() => {
    const el = ref.current;
    if (visible || !el || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        setVisible(true);
        io.disconnect();
      }
    });
    io.observe(el);
    return () => io.disconnect();
  }, [visible]);
  const { data } = useResource<History>(visible ? key : null, () => getHistory(repo.owner, repo.name, sha, '', { perPage: 1 }), { immutable: true });
  const c = data?.commits[0];
  return <span ref={ref}>{c ? <RelativeTime date={c.committer.date || c.author.date} /> : visible && !data ? <Skeleton width={70} height={12} /> : null}</span>;
}

function SkeletonTags() {
  return (
    <section className={styles.section} aria-busy="true">
      <div className={styles.box}>
        {Array.from({ length: 6 }, (_, i) => (
          <div key={i} className={styles.tagRow}>
            <div>
              <Skeleton width={120} />
              <div className={styles.tagMeta}>
                <Skeleton width={200} height={12} />
              </div>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
