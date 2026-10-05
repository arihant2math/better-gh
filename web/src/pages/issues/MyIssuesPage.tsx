import { observer } from 'mobx-react-lite';
import { useCallback } from 'react';
import { session } from '../../app/session';
import { useLocation, useQuery } from '../../router';
import { store } from '../../sync';
import { TabNav } from '../../ui/Tabs';
import { IssueList } from './IssueList';
import styles from './IssueList.module.css';

const ISSUE_TABS = [
  { id: 'assigned', label: 'Assigned', q: 'is:open assignee:@me' },
  { id: 'created', label: 'Created', q: 'is:open author:@me' },
];
const PR_TABS = [
  { id: 'review', label: 'Review requests', q: 'is:open review-requested:@me' },
  { id: 'created', label: 'Created', q: 'is:open author:@me' },
  { id: 'assigned', label: 'Assigned', q: 'is:open assignee:@me' },
];

/** Cross-repo lists (/issues, /pulls) — same IssueList, different source. */
export default observer(function MyIssuesPage() {
  const { pathname } = useLocation();
  const kind = pathname.startsWith('/pulls') ? 'pr' : 'issue';
  const tabs = kind === 'pr' ? PR_TABS : ISSUE_TABS;
  const current = useQuery().get('q') ?? tabs[0]!.q;
  const tab = tabs.find((t) => current.startsWith(t.q)) ?? tabs[0]!;
  const viewer = session.user!.id;
  const source = useCallback(() => {
    const s = store();
    const isPr = kind === 'pr';
    // Narrow with indexes when possible; fall back to a full scan (fast enough locally).
    if (tab.id === 'assigned') return s.byIndex('issue', 'assigneeIds', viewer).filter((i) => i.isPr === isPr);
    if (tab.id === 'created') return s.byIndex('issue', 'authorId', viewer).filter((i) => i.isPr === isPr);
    return s.all('issue').filter((i) => i.isPr === isPr);
  }, [kind, tab.id, viewer]);

  return (
    <IssueList
      key={kind}
      kind={kind}
      source={source}
      showRepo
      defaultQuery={tabs[0]!.q}
      emptyTitle={kind === 'pr' ? 'Nothing to review — nice.' : 'Nothing assigned to you'}
      header={
        <div className={styles.pageHeader}>
          <h1 className={styles.pageTitle}>{kind === 'pr' ? 'Pull requests' : 'Issues'}</h1>
          <TabNav
            aria-label="Views"
            current={tab.id}
            items={tabs.map((t) => ({ id: t.id, label: t.label, href: `${pathname}?q=${encodeURIComponent(t.q)}` }))}
          />
        </div>
      }
    />
  );
});
