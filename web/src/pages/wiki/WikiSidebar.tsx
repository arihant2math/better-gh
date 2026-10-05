import { useState } from 'react';
import { useResource } from '../../api/cache';
import { searchWiki, wikiKey } from '../../api/wiki';
import { Link } from '../../router';
import { Counter } from '../../ui/Badge';
import { SearchIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { useWikiIndex } from './data';
import { WikiHtml } from './WikiHtml';
import styles from './Wiki.module.css';

export function WikiSidebar({ owner, repo, current, sidebarHtml }: { owner: string; repo: string; current?: string; sidebarHtml?: string }) {
  const index = useWikiIndex(owner, repo);
  const [q, setQ] = useState('');
  const [submitted, setSubmitted] = useState('');
  const results = useResource(submitted ? wikiKey(owner, repo, 'search', submitted) : null, () => searchWiki(owner, repo, submitted));
  const base = `/${owner}/${repo}/wiki`;
  const pages = index.data?.pages ?? [];
  const filtered = q && !submitted ? pages.filter((p) => p.title.toLowerCase().includes(q.toLowerCase())) : pages;
  return (
    <aside className={styles.sidebar} aria-label="Wiki navigation">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          setSubmitted(q.trim());
        }}
      >
        <Input
          size="sm"
          leadingIcon={SearchIcon}
          value={q}
          placeholder="Find a page… (Enter: full text)"
          aria-label="Search wiki"
          onChange={(e) => {
            setQ(e.target.value);
            if (!e.target.value) setSubmitted('');
          }}
        />
      </form>
      {submitted ? (
        <section className={styles.box}>
          <h2 className={styles.boxTitle}>
            Results for “{submitted}” <Counter>{results.data?.results.length ?? '…'}</Counter>
          </h2>
          <ul className={styles.pageList}>
            {(results.data?.results ?? []).map((r) => (
              <li key={r.slug}>
                <Link to={`${base}/${r.slug}`}>{r.title}</Link>
                <p className={styles.snippet}>{r.snippet}</p>
              </li>
            ))}
            {results.data && results.data.results.length === 0 && <li className={styles.muted}>No pages found</li>}
          </ul>
        </section>
      ) : null}
      <section className={styles.box}>
        <h2 className={styles.boxTitle}>
          Pages <Counter>{pages.length}</Counter>
        </h2>
        <ul className={styles.pageList}>
          {filtered.map((p) => (
            <li key={p.slug}>
              <Link to={`${base}/${p.slug}`} aria-current={p.slug.toLowerCase() === current?.toLowerCase() ? 'page' : undefined}>
                {p.title}
              </Link>
            </li>
          ))}
        </ul>
      </section>
      {sidebarHtml && (
        <section className={styles.box}>
          <WikiHtml html={sidebarHtml} className={styles.sidebarHtml} />
        </section>
      )}
    </aside>
  );
}
