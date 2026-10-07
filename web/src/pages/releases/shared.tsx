/* Shared pieces of the releases pages: release card, assets, badges, helpers. */
import { observer } from 'mobx-react-lite';
import type { MouseEvent, ReactNode } from 'react';
import { invalidate, prefetch, useResource, type ResourceState } from '../../api/cache';
import { codeKeys, findReleaseByTag, getLatestRelease, type RestAsset, type RestRelease } from '../../api/code';
import { browseKeys } from '../../api/endpoints';
import { useRefs } from '../../components/code/RefPicker';
import { Link, navigate, prefetch as prefetchRoute } from '../../router';
import { archiveUrl, treeUrl } from '../../components/code/urls';
import { store } from '../../sync';
import { repoByName } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button, cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { DownloadIcon, FileZipIcon, GitCommitIcon, PackageIcon, TagIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { RelativeTime } from '../../ui/RelativeTime';
import '../../ui/markdown.css';
import styles from './Releases.module.css';

export const PER_PAGE = 20;

export const releasesBase = (o: string, r: string) => `/${o}/${r}/releases`;
export const releaseHref = (o: string, r: string, tag: string) => `${releasesBase(o, r)}/tag/${encodeURIComponent(tag)}`;
export const editHref = (o: string, r: string, tag: string) => `${releasesBase(o, r)}/edit/${encodeURIComponent(tag)}`;

/** Push access (admin / maintain / write). Call from an observer. */
export function useCanPush(owner: string, name: string): boolean {
  const repo = repoByName(owner, name);
  const p = repo ? store().get('viewerRepo', repo.id)?.permission : undefined;
  return p === 'admin' || p === 'maintain' || p === 'write';
}

const NO_LATEST: ResourceState<RestRelease | null> = { data: null, error: undefined, loading: false };

/** A repository that was never pushed has no commits, so no releases: `releases/latest` would only 404. */
export function skipLatestRelease(repo: { pushedAt: string | null } | undefined): boolean {
  return !!repo && !repo.pushedAt;
}

/** `releases/latest`; `data` is `null` when there is none. Call from an observer. */
export function useLatestRelease(owner: string, repo: string): ResourceState<RestRelease | null> {
  const skip = skipLatestRelease(repoByName(owner, repo));
  const res = useResource(skip ? null : codeKeys.latestRelease(owner, repo), () => getLatestRelease(owner, repo));
  return skip ? NO_LATEST : res;
}

/** Drop every cached release list/detail (+ tags, which publishing may create). */
export function invalidateReleases(owner: string, repo: string): void {
  invalidate(`releases:${owner}/${repo}#`);
  invalidate(`release:${owner}/${repo}@`);
  invalidate(codeKeys.latestRelease(owner, repo));
  invalidate(codeKeys.tags(owner, repo));
  invalidate(browseKeys.refs(owner, repo));
}

/** Seed the detail cache with a just-saved release so navigation is instant. */
export function primeRelease(owner: string, repo: string, rel: RestRelease): void {
  prefetch(codeKeys.release(owner, repo, rel.tag_name), () => Promise.resolve(rel));
}

export function prefetchRelease(owner: string, repo: string, tag: string): void {
  prefetch(codeKeys.release(owner, repo, tag), () => findReleaseByTag(owner, repo, tag));
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} Bytes`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? Math.round(v) : v.toFixed(v >= 10 ? 1 : 2).replace(/\.?0+$/, '')} ${units[i]}`;
}

export function isNotFound(e: unknown): boolean {
  return typeof e === 'object' && e !== null && (e as { status?: number }).status === 404;
}

// ------------------------------------------------------------------ badges

export function Pill({ kind, children }: { kind: 'latest' | 'pre' | 'draft'; children: ReactNode }) {
  return <span className={cx(styles.pill, styles[`pill_${kind}`])}>{children}</span>;
}

export function ReleaseBadges({ release, latest }: { release: RestRelease; latest: boolean }) {
  return (
    <>
      {release.draft && <Pill kind="draft">Draft</Pill>}
      {release.prerelease && <Pill kind="pre">Pre-release</Pill>}
      {latest && !release.draft && <Pill kind="latest">Latest</Pill>}
    </>
  );
}

// ------------------------------------------------------------------ body

/** Server-rendered (sanitized) `body_html`, internal links go through the router. */
function Html({ html }: { html: string }) {
  const onClick = (e: MouseEvent<HTMLDivElement>) => {
    const a = (e.target as HTMLElement).closest('a');
    if (!a || a.target || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    const href = a.getAttribute('href');
    if (href?.startsWith('/')) {
      e.preventDefault();
      navigate(href);
    }
  };
  const onOver = (e: MouseEvent<HTMLDivElement>) => {
    const href = (e.target as HTMLElement).closest('a')?.getAttribute('href');
    if (href?.startsWith('/')) prefetchRoute(href);
  };
  return <div className="markdown-body" onClick={onClick} onPointerOver={onOver} dangerouslySetInnerHTML={{ __html: html }} />;
}

export function ReleaseBody({ release, repo }: { release: RestRelease; repo: string }) {
  if (release.body_html !== undefined && (release.body_html || !release.body)) {
    return release.body_html ? <Html html={release.body_html} /> : <div className={cx('markdown-body', styles.noBody)}>No release notes.</div>;
  }
  if (!release.body?.trim()) return <div className={cx('markdown-body', styles.noBody)}>No release notes.</div>;
  return <Markdown source={release.body} repo={repo} />;
}

// ------------------------------------------------------------------ assets

export function AssetRow({ asset, trailing }: { asset: RestAsset; trailing?: ReactNode }) {
  return (
    <li className={styles.asset}>
      <PackageIcon size={16} className={styles.assetIcon} />
      <a className={styles.assetName} href={asset.browser_download_url} download={asset.name} rel="nofollow">
        {asset.name}
      </a>
      {asset.label && <span className={styles.muted}>{asset.label}</span>}
      <span className={styles.assetMeta}>
        <span>{formatBytes(asset.size)}</span>
        <span className={styles.downloads} title={`${asset.download_count} downloads`}>
          <DownloadIcon size={12} />
          {asset.download_count.toLocaleString()}
        </span>
        <span className={styles.hideNarrow}>
          <RelativeTime date={asset.created_at} short />
        </span>
      </span>
      {trailing}
    </li>
  );
}

export function Assets({ owner, repo, release, open }: { owner: string; repo: string; release: RestRelease; open: boolean }) {
  const source = !release.draft;
  const count = release.assets.length + (source ? 2 : 0);
  if (!count) return null;
  const archive = (ext: 'zip' | 'tar.gz') => archiveUrl({ owner, repo }, `refs/tags/${release.tag_name}`, ext);
  return (
    <details className={styles.assets} open={open}>
      <summary className={styles.assetsSummary}>
        <span className={styles.assetsTitle}>Assets</span>
        <span className={styles.counter}>{count}</span>
      </summary>
      <ul className={styles.assetList}>
        {release.assets.map((a) => (
          <AssetRow key={a.id} asset={a} />
        ))}
        {source &&
          (['zip', 'tar.gz'] as const).map((ext) => (
            <li key={ext} className={styles.asset}>
              <FileZipIcon size={16} className={styles.assetIcon} />
              <a className={styles.assetName} href={archive(ext)} rel="nofollow">
                <strong>Source code</strong> ({ext})
              </a>
            </li>
          ))}
      </ul>
    </details>
  );
}

// ------------------------------------------------------------------ card

/** Tag → commit SHA map from the shared refs list (code browser cache). */
export function useTagShas(owner: string, repo: string): Map<string, string> {
  const refs = useRefs(owner, repo);
  return new Map((refs.data?.tags ?? []).map((t) => [t.name, t.sha]));
}

export const ReleaseMeta = observer(function ReleaseMeta({
  owner,
  repo,
  release,
  sha,
}: {
  owner: string;
  repo: string;
  release: RestRelease;
  sha: string | undefined;
}) {
  const when = release.published_at ?? release.created_at;
  return (
    <div className={styles.meta}>
      <span className={styles.metaItem}>
        <RelativeTime date={when} />
      </span>
      {release.author && (
        <Link to={`/${release.author.login}`} className={styles.metaItem}>
          <Avatar user={{ login: release.author.login, avatarUrl: release.author.avatar_url }} size={18} />
          {release.author.login}
        </Link>
      )}
      {sha ? (
        <Link to={treeUrl({ owner, repo }, release.tag_name)} className={cx(styles.metaItem, styles.mono)}>
          <TagIcon size={14} />
          {release.tag_name}
        </Link>
      ) : (
        <span className={cx(styles.metaItem, styles.mono)} title="This tag will be created when the release is published">
          <TagIcon size={14} />
          {release.tag_name}
        </span>
      )}
      {sha ? (
        <Link to={`/${owner}/${repo}/commit/${sha}`} className={cx(styles.metaItem, styles.mono)}>
          <GitCommitIcon size={14} />
          {sha.slice(0, 7)}
        </Link>
      ) : (
        <span className={cx(styles.metaItem, styles.muted)}>
          target: <span className={styles.mono}>{release.target_commitish}</span>
        </span>
      )}
    </div>
  );
});

export function ReleaseCard({
  owner,
  repo,
  release,
  latest,
  sha,
  linkTitle,
  assetsOpen,
  actions,
}: {
  owner: string;
  repo: string;
  release: RestRelease;
  latest: boolean;
  sha: string | undefined;
  linkTitle: boolean;
  assetsOpen: boolean;
  actions?: ReactNode;
}) {
  const title = release.name?.trim() || release.tag_name;
  const href = releaseHref(owner, repo, release.tag_name);
  return (
    <article className={styles.card}>
      <header className={styles.cardHead}>
        <div className={styles.cardTitleRow}>
          {linkTitle ? (
            <h2 className={styles.cardTitle}>
              <Link to={href} onMouseEnter={() => prefetchRelease(owner, repo, release.tag_name)}>
                {title}
              </Link>
            </h2>
          ) : (
            <h1 className={styles.cardTitle}>{title}</h1>
          )}
          <ReleaseBadges release={release} latest={latest} />
          {actions && <div className={styles.cardActions}>{actions}</div>}
        </div>
        <ReleaseMeta owner={owner} repo={repo} release={release} sha={sha} />
      </header>
      <div className={styles.cardBody}>
        <ReleaseBody release={release} repo={`${owner}/${repo}`} />
      </div>
      <Assets owner={owner} repo={repo} release={release} open={assetsOpen} />
    </article>
  );
}

// ------------------------------------------------------------------ page chrome

export function ReleasesHeader({ owner, repo, current, canPush }: { owner: string; repo: string; current: 'releases' | 'tags'; canPush: boolean }) {
  return (
    <header className={styles.head}>
      <nav className={styles.subnav} aria-label="Releases and tags">
        <Link to={releasesBase(owner, repo)} className={styles.subnavItem} aria-current={current === 'releases' ? 'page' : undefined}>
          Releases
        </Link>
        <Link to={`/${owner}/${repo}/tags`} className={styles.subnavItem} aria-current={current === 'tags' ? 'page' : undefined}>
          Tags
        </Link>
      </nav>
      {canPush && (
        <Button
          variant="primary"
          kbd="C"
          onClick={() => navigate(`${releasesBase(owner, repo)}/new`)}
          onMouseEnter={() => prefetchRoute(`${releasesBase(owner, repo)}/new`)}
        >
          Draft a new release
        </Button>
      )}
    </header>
  );
}

export function CardSkeletons({ count }: { count: number }) {
  return (
    <>
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className={styles.card} aria-hidden>
          <div className={styles.cardHead}>
            <Skeleton width="30%" height={22} />
            <Skeleton width="45%" height={14} style={{ marginTop: 8 }} />
          </div>
          <div className={styles.cardBody}>
            <Skeleton width="90%" />
            <Skeleton width="70%" style={{ marginTop: 8 }} />
            <Skeleton width="80%" style={{ marginTop: 8 }} />
          </div>
        </div>
      ))}
    </>
  );
}
