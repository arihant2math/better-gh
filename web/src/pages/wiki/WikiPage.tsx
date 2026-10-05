import { observer } from 'mobx-react-lite';
import { Link, navigate, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { BookIcon, HistoryIcon, PencilIcon, PlusIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { WikiSidebar } from './WikiSidebar';
import { isNotFound, useWikiIndex, useWikiPage } from './data';
import { WikiHtml } from './WikiHtml';
import styles from './Wiki.module.css';

/** `/:owner/:repo/wiki` (Home) and `/:owner/:repo/wiki/:slug` (`?rev=` for a revision). */
export default observer(function WikiPage() {
  const { owner, repo, slug: slugParam } = useParams<{ owner: string; repo: string; slug?: string }>();
  const rev = useQuery().get('rev');
  const index = useWikiIndex(owner, repo);
  const slug = slugParam ?? index.data?.home ?? (index.data ? 'Home' : null);
  const page = useWikiPage(owner, repo, index.data?.exists === false ? null : slug, rev);
  const base = `/${owner}/${repo}/wiki`;
  const canEdit = !!index.data?.canEdit;

  useShortcuts('Wiki', {
    e: { handler: () => (canEdit && page.data ? navigate(`${base}/${page.data.slug}/edit`) : false), description: 'Edit page', group: 'Wiki' },
    c: { handler: () => (canEdit ? navigate(`${base}/new`) : false), description: 'New page', group: 'Wiki' },
    h: { handler: () => (page.data ? navigate(`${base}/${page.data.slug}/history`) : false), description: 'Page history', group: 'Wiki' },
  });

  if (index.error && isNotFound(index.error)) {
    return <EmptyState icon={BookIcon} title="This repository has no wiki" />;
  }
  if (index.data && !index.data.exists) {
    return (
      <EmptyState
        icon={BookIcon}
        title="Welcome to the wiki!"
        action={
          canEdit ? (
            <Button variant="primary" onClick={() => navigate(`${base}/new?title=Home`)}>
              Create the first page
            </Button>
          ) : undefined
        }
      >
        Wikis provide a place in your repository to lay out the roadmap of your project, show the current status, and document software better, together.
      </EmptyState>
    );
  }

  const missing = page.error && isNotFound(page.error);
  const p = page.data;
  return (
    <div className={styles.layout}>
      <article className={styles.main}>
        <header className={styles.pageHead}>
          <div>
            <h1 className={styles.h1}>{p?.title ?? slug?.replace(/-/g, ' ') ?? <Skeleton width={200} height={24} />}</h1>
            {p && (
              <div className={styles.meta}>
                <Avatar
                  user={{ login: p.commit.author.login ?? p.commit.author.name, avatarUrl: p.commit.author.avatarUrl ?? '', name: p.commit.author.name }}
                  size={18}
                />
                <strong>{p.commit.author.login ?? p.commit.author.name}</strong> edited this page <RelativeTime date={p.commit.date} /> ·{' '}
                <Link to={`${base}/${p.slug}/history`}>History</Link>
              </div>
            )}
          </div>
          <div className={styles.actions}>
            {p && (
              <Button size="sm" leadingIcon={HistoryIcon} onClick={() => navigate(`${base}/${p.slug}/history`)}>
                History
              </Button>
            )}
            {canEdit && p && !rev && (
              <Button size="sm" leadingIcon={PencilIcon} kbd="E" onClick={() => navigate(`${base}/${p.slug}/edit`)}>
                Edit
              </Button>
            )}
            {canEdit && (
              <Button size="sm" variant="primary" leadingIcon={PlusIcon} onClick={() => navigate(`${base}/new`)}>
                New page
              </Button>
            )}
          </div>
        </header>
        {rev && p && (
          <div className={styles.banner}>
            Viewing this page as of <code>{rev.slice(0, 7)}</code>. <Link to={`${base}/${p.slug}`}>View the latest version</Link>
          </div>
        )}
        {missing ? (
          <EmptyState
            icon={BookIcon}
            title="This page does not exist yet"
            action={
              canEdit ? (
                <Button variant="primary" onClick={() => navigate(`${base}/new?title=${encodeURIComponent(slug ?? '')}`)}>
                  Create {slug?.replace(/-/g, ' ')}
                </Button>
              ) : undefined
            }
          />
        ) : p ? (
          <WikiHtml html={p.html} />
        ) : page.error ? (
          <EmptyState icon={BookIcon} title="Could not load this page">
            {String((page.error as Error).message ?? page.error)}
          </EmptyState>
        ) : (
          <div className={styles.skeletons}>
            <Skeleton width="80%" />
            <Skeleton width="95%" />
            <Skeleton width="60%" />
          </div>
        )}
        {(p?.footer ?? index.data?.footer) && (
          <footer className={styles.footer}>
            <WikiHtml html={(p?.footer ?? index.data!.footer)!.html} />
          </footer>
        )}
      </article>
      <WikiSidebar owner={owner} repo={repo} current={p?.slug ?? slug ?? undefined} sidebarHtml={(p?.sidebar ?? index.data?.sidebar)?.html} />
    </div>
  );
});
