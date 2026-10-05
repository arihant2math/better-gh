import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { useResource } from '../../api/cache';
import { getIssueLinks } from '../../api/endpoints';
import type { IssueLinkItem, IssueLinks } from '../../api/types';
import { Link } from '../../router';
import { store } from '../../sync';
import type { Issue, Repo } from '../../sync/models';
import { commit, enc, repoOf } from '../../sync/mutations';
import { ops } from '../../sync/overlay';
import { canPush, issuesForRepo } from '../../sync/selectors';
import { StateIcon } from '../../ui/Badge';
import { GearIcon, GitBranchIcon } from '../../ui/icons';
import { SelectPanel } from '../../ui/Menu';
import styles from './IssueView.module.css';

/**
 * Link (`on`) or unlink an issue and a pull request through the `/_bgh`
 * link API of `here` (either side). Optimistic on both rows; keyword links
 * can't be removed by hand (the server answers 422 and the change rolls back).
 */
export function setIssueLink(here: Issue, there: Issue, on: boolean) {
  const [issue, pr] = here.isPr ? [there, here] : [here, there];
  const r = repoOf(here.repoId);
  const t = repoOf(there.repoId);
  const path = `/_bgh/repos/${enc(r.owner)}/${enc(r.name)}/issues/${here.number}/links`;
  return commit(
    on ? `Link #${there.number}` : `Unlink #${there.number}`,
    [
      ops.update('issue', issue.id, { linkedPullIds: on ? { $add: [pr.id] } : { $remove: [pr.id] } }),
      ops.update('issue', pr.id, { closingIssueIds: on ? { $add: [issue.id] } : { $remove: [issue.id] } }),
    ],
    on
      ? { method: 'POST', path, body: { repository: `${t.owner}/${t.name}`, number: there.number } }
      : { method: 'DELETE', path: `${path}/${there.id}` },
  );
}

interface Row {
  id: number;
  href: string;
  label: string;
  title: string;
  visual: Pick<Issue, 'isPr' | 'state' | 'stateReason' | 'merged' | 'draft'>;
}

/** A linked item from the local store when synced (live state), else from the links endpoint. */
function rowFor(id: number, fetched: IssueLinkItem | undefined, current: Repo): Row | undefined {
  const s = store();
  const local = s.get('issue', id);
  const localRepo = local && s.get('repo', local.repoId);
  if (local && localRepo) {
    const full = `${localRepo.owner}/${localRepo.name}`;
    return {
      id,
      href: `/${full}/${local.isPr ? 'pull' : 'issues'}/${local.number}`,
      label: localRepo.id === current.id ? `#${local.number}` : `${full}#${local.number}`,
      title: local.title,
      visual: local,
    };
  }
  if (!fetched) return undefined;
  const same = fetched.repoId === current.id;
  return {
    id,
    href: `/${fetched.repository}/${fetched.isPr ? 'pull' : 'issues'}/${fetched.number}`,
    label: same ? `#${fetched.number}` : `${fetched.repository}#${fetched.number}`,
    title: fetched.title,
    visual: fetched,
  };
}

/**
 * Sidebar "Development": for an issue, the pull requests that close it and
 * its linked branches; for a pull request, the issues it closes on merge.
 * Ids come from the synced row (`linkedPullIds` / `closingIssueIds`), so
 * the section updates live; titles of items in other repositories come
 * from `/_bgh/.../links` (refetched whenever the ids change).
 */
export const DevelopmentSection = observer(function DevelopmentSection({ issue, repo }: { issue: Issue; repo: Repo }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const ids = issue.isPr ? issue.closingIssueIds : issue.linkedPullIds;
  const key = issue.id > 0 ? `links:${repo.owner}/${repo.name}#${issue.number}:${(ids ?? []).join(',')}` : null;
  const res = useResource<IssueLinks>(key, () => getIssueLinks(repo.owner, repo.name, issue.number));
  const last = useRef<IssueLinks | undefined>(undefined);
  if (res.data) last.current = res.data;
  const fetched = res.data ?? last.current;
  const byId = new Map((fetched?.links ?? []).map((l) => [l.id, l]));
  // Rows without synced ids (old cached rows) fall back to the endpoint's list.
  const order = ids ?? fetched?.links.map((l) => l.id) ?? [];
  const rows = order.map((id) => rowFor(id, byId.get(id), repo)).filter((r): r is Row => r !== undefined);
  const writable = canPush(repo.id) && issue.id > 0;
  const branches = fetched?.branches ?? [];
  const title = 'Development';

  const candidates = issuesForRepo(repo.id)
    .filter((i) => i.isPr !== issue.isPr && i.id > 0 && (i.state === 'open' || (ids ?? []).includes(i.id)))
    .sort((a, b) => b.number - a.number)
    .slice(0, 100);

  return (
    <section className={styles.sideSection} data-testid="development">
      {writable ? (
        <button ref={anchor} type="button" className={styles.sideHeader} onClick={() => setOpen(true)} title={issue.isPr ? 'Link an issue' : 'Link a pull request'}>
          {title}
          <GearIcon size={14} />
        </button>
      ) : (
        <div className={styles.sideHeaderStatic}>{title}</div>
      )}
      <div className={styles.sideBody}>
        {issue.isPr && rows.length > 0 && <p className={styles.devNote}>Successfully merging this pull request may close these issues.</p>}
        {rows.map((r) => (
          <Link key={r.id} to={r.href} className={styles.devItem}>
            <StateIcon issue={r.visual} size={14} />
            <span className={styles.devTitle}>{r.title}</span>
            <span className={styles.subtle}>{r.label}</span>
          </Link>
        ))}
        {branches.map((b) => (
          <Link key={b.name} to={`/${repo.owner}/${repo.name}/tree/${b.name.split('/').map(encodeURIComponent).join('/')}`} className={styles.devItem}>
            <GitBranchIcon size={14} />
            <span className={styles.devTitle}>{b.name}</span>
          </Link>
        ))}
        {rows.length === 0 && branches.length === 0 && (
          <span className={styles.subtle}>
            {writable ? (
              <button type="button" className={styles.linkButton} onClick={() => setOpen(true)}>
                {issue.isPr ? 'Link an issue' : 'Link a pull request'}
              </button>
            ) : issue.isPr ? (
              'No linked issues'
            ) : (
              'No branches or pull requests'
            )}
          </span>
        )}
      </div>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        placement="bottom-end"
        title={issue.isPr ? 'Link an issue from this repository' : 'Link a pull request from this repository'}
        placeholder={issue.isPr ? 'Search issues' : 'Search pull requests'}
        emptyText={issue.isPr ? 'No open issues' : 'No open pull requests'}
        items={candidates.map((i) => ({
          id: i.id,
          text: `#${i.number} ${i.title}`,
          leading: <StateIcon issue={i} size={14} />,
          selected: (ids ?? []).includes(i.id),
        }))}
        onToggle={(id) => {
          const there = store().get('issue', Number(id));
          if (there) setIssueLink(issue, there, !(ids ?? []).includes(there.id));
        }}
      />
    </section>
  );
});
