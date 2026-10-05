import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { codeKeys, getLanguages, getLatestRelease, listContributors, type RestContributor, type RestRelease } from '../../api/code';
import { DeploymentsSidebar } from '../deployments/DeploymentsSidebar';
import { Link } from '../../router';
import type { Repo } from '../../sync/models';
import { Avatar } from '../../ui/Badge';
import { Skeleton } from '../../ui/EmptyState';
import { BookIcon, EyeIcon, LawIcon, LinkIcon, RepoForkedIcon, StarIcon, TagIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Code.module.css';
import { useFullRepo } from './CloneMenu';
import { languageColor } from './languages';

/** Repository home sidebar: about, topics, stats, latest release, contributors, languages. */
export const AboutSidebar = observer(function AboutSidebar({ repo }: { repo: Repo }) {
  const { owner, name } = repo;
  const full = useFullRepo(owner, name);
  const release = useResource<RestRelease | null>(codeKeys.latestRelease(owner, name), () => getLatestRelease(owner, name), { ttlMs: 60_000 });
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
          <li>
            <BookIcon size={16} /> <a href="#readme">Readme</a>
          </li>
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
        {release.data === undefined ? (
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
