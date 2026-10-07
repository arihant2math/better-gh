import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { listRepoPackages, packageHref, packageKeys, packagesHref } from '../../api/packages';
import { codeKeys, getLanguages, getLatestRelease, listContributors, type RestContributor, type RestRelease } from '../../api/code';
import { DeploymentsSidebar } from '../deployments/DeploymentsSidebar';
import { Link } from '../../router';
import type { Repo } from '../../sync/models';
import { Avatar } from '../../ui/Badge';
import { Skeleton } from '../../ui/EmptyState';
import { BookIcon, EyeIcon, LawIcon, LinkIcon, PackageIcon, RepoForkedIcon, StarIcon, TagIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Code.module.css';
import { useFullRepo } from './CloneMenu';
import { languageColor } from './languages';

/**
 * Whether to fetch `releases/latest`. A repository without commits has no
 * releases, so the probe (a 404) is skipped; a never-pushed repository waits
 * for the root tree to say whether it is empty.
 */
export function probeLatestRelease(loaded: boolean, empty: boolean | undefined, pushedAt: string | null): boolean {
  if (empty) return false;
  return loaded || !!pushedAt;
}

/**
 * Repository home sidebar: about, topics, stats, latest release, contributors, languages.
 * `loaded`/`empty`/`hasReadme` come from the root tree listing.
 */
export const AboutSidebar = observer(function AboutSidebar({ repo, loaded, empty, hasReadme }: { repo: Repo; loaded: boolean; empty?: boolean; hasReadme: boolean }) {
  const { owner, name } = repo;
  const full = useFullRepo(owner, name);
  const release = useResource<RestRelease | null>(!probeLatestRelease(loaded, empty, repo.pushedAt) ? null : codeKeys.latestRelease(owner, name), () => getLatestRelease(owner, name), { ttlMs: 60_000 });
  const contributors = useResource<RestContributor[]>(codeKeys.contributors(owner, name), () => listContributors(owner, name), { ttlMs: 300_000 });
  const languages = useResource<Record<string, number>>(codeKeys.languages(owner, name), () => getLanguages(owner, name), { ttlMs: 300_000 });
  const base = `/${owner}/${name}`;
  const homepage = full.data?.homepage;
  const topics = full.data?.topics ?? repo.topics;
  return (
    <aside className={styles.about} aria-label="About">
      <section className={styles.aboutSection}>
        <h2 className={styles.aboutTitle}>About</h2>
        {repo.description ? <p className={styles.aboutDesc}>{repo.description}</p> : <p className={styles.muted}>No description, website, or topics provided.</p>}
        {homepage && (
          <a className={styles.aboutLink} href={/^https?:\/\//.test(homepage) ? homepage : `https://${homepage}`} target="_blank" rel="noopener noreferrer nofollow">
            <LinkIcon size={16} /> <span>{homepage.replace(/^https?:\/\//, '')}</span>
          </a>
        )}
        {topics.length > 0 && (
          <div className={styles.topics}>
            {topics.map((t) => (
              <span key={t} className={styles.topic}>
                {t}
              </span>
            ))}
          </div>
        )}
        <ul className={styles.aboutStats}>
          {hasReadme && (
            <li>
              <BookIcon size={16} /> <a href="#readme">Readme</a>
            </li>
          )}
          {full.data?.license && (
            <li>
              <LawIcon size={16} /> {full.data.license.spdx_id && full.data.license.spdx_id !== 'NOASSERTION' ? `${full.data.license.spdx_id} license` : full.data.license.name}
            </li>
          )}
          <li>
            <StarIcon size={16} /> <strong>{repo.stars.toLocaleString()}</strong> stars
          </li>
          <li>
            <EyeIcon size={16} /> <strong>{repo.watchers.toLocaleString()}</strong> watching
          </li>
          <li>
            <RepoForkedIcon size={16} /> <strong>{repo.forks.toLocaleString()}</strong> forks
          </li>
        </ul>
      </section>

      <section className={styles.aboutSection}>
        <h2 className={styles.aboutTitle}>
          <Link to={`${base}/releases`}>Releases</Link>
        </h2>
        {empty ? (
          <p className={styles.muted}>No releases published</p>
        ) : release.data === undefined ? (
          <Skeleton width="70%" />
        ) : release.data ? (
          <Link to={`${base}/releases/tag/${encodeURIComponent(release.data.tag_name)}`} className={styles.release}>
            <TagIcon size={16} className={styles.releaseIcon} />
            <span>
              <strong>{release.data.name || release.data.tag_name}</strong> <span className={styles.latest}>Latest</span>
              <br />
              <span className={styles.muted}>
                <RelativeTime date={release.data.published_at ?? release.data.created_at} />
              </span>
            </span>
          </Link>
        ) : (
          <p className={styles.muted}>
            No releases published. <Link to={`${base}/releases/new`}>Create a new release</Link>
          </p>
        )}
      </section>

      <DeploymentsSidebar owner={owner} repo={name} sectionClass={styles.aboutSection} titleClass={styles.aboutTitle} />

      <Packages owner={owner} name={name} />

      {contributors.data && contributors.data.length > 0 && (
        <section className={styles.aboutSection}>
          <h2 className={styles.aboutTitle}>
            Contributors <span className={styles.count}>{contributors.data.length}</span>
          </h2>
          <div className={styles.contributors}>
            {contributors.data.slice(0, 14).map((c, i) =>
              c.login ? (
                <Link key={c.login} to={`/${c.login}`} title={`${c.login} · ${c.contributions} commits`}>
                  <Avatar user={{ login: c.login, avatarUrl: c.avatar_url ?? '' }} size={32} />
                </Link>
              ) : (
                <span key={`anon-${i}`} title={`${c.name ?? 'anonymous'} · ${c.contributions} commits`}>
                  <Avatar user={{ login: c.name ?? '?', avatarUrl: '' }} size={32} />
                </span>
              ),
            )}
          </div>
        </section>
      )}

      {languages.data && Object.keys(languages.data).length > 0 && <Languages data={languages.data} />}
    </aside>
  );
});

/** "Packages" (container images linked to this repository); loads lazily, never blocks the page. */
function Packages({ owner, name }: { owner: string; name: string }) {
  const res = useResource(packageKeys.repo(owner, name), () => listRepoPackages(owner, name), { ttlMs: 60_000 });
  // The endpoint may be unavailable (older server): hide the section rather than show an error.
  if (res.error && !res.data) return null;
  const pkgs = res.data?.packages;
  return (
    <section className={styles.aboutSection} aria-label="Packages">
      <h2 className={styles.aboutTitle}>
        Packages {pkgs && pkgs.length > 0 && <span className={styles.count}>{pkgs.length}</span>}
      </h2>
      {!pkgs ? (
        <Skeleton width="60%" />
      ) : pkgs.length === 0 ? (
        <p className={styles.muted}>No packages published</p>
      ) : (
        <ul className={styles.packages}>
          {pkgs.slice(0, 5).map((p) => (
            <li key={p.id}>
              <PackageIcon size={16} className={styles.packageIcon} />
              <Link to={packageHref(p)}>{p.name}</Link>
            </li>
          ))}
          {pkgs.length > 5 && (
            <li>
              <Link to={packagesHref(pkgs[0]!.owner)}>+ {pkgs.length - 5} more packages</Link>
            </li>
          )}
        </ul>
      )}
    </section>
  );
}

function Languages({ data }: { data: Record<string, number> }) {
  const total = Object.values(data).reduce((a, b) => a + b, 0) || 1;
  const entries = Object.entries(data)
    .sort((a, b) => b[1] - a[1])
    .map(([lang, bytes]) => ({ lang, pct: (bytes / total) * 100 }));
  const shown = entries.filter((e) => e.pct >= 0.1).slice(0, 8);
  const other = 100 - shown.reduce((a, e) => a + e.pct, 0);
  if (other >= 0.1) shown.push({ lang: 'Other', pct: other });
  return (
    <section className={styles.aboutSection}>
      <h2 className={styles.aboutTitle}>Languages</h2>
      <div className={styles.langBar} role="img" aria-label={shown.map((e) => `${e.lang} ${e.pct.toFixed(1)}%`).join(', ')}>
        {shown.map((e) => (
          <span key={e.lang} style={{ width: `${e.pct}%`, background: languageColor(e.lang) }} title={`${e.lang} ${e.pct.toFixed(1)}%`} />
        ))}
      </div>
      <ul className={styles.langList}>
        {shown.map((e) => (
          <li key={e.lang}>
            <span className={styles.langDot} style={{ background: languageColor(e.lang) }} />
            <strong>{e.lang}</strong> <span className={styles.muted}>{e.pct.toFixed(1)}%</span>
          </li>
        ))}
      </ul>
    </section>
  );
}
