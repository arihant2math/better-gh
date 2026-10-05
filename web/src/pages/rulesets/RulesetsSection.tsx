/**
 * Rulesets UI shared by repository settings (`/:owner/:repo/settings/rules`)
 * and organization settings (`/organizations/:org/settings/rules`):
 *
 *   rules              list (+ import / export)
 *   rules/new?target=  new ruleset (`?import=1` prefilled from an imported file)
 *   rules/:id          edit
 *   rules/insights     rule suites (`?suite=id` opens one)
 */
import { observer } from 'mobx-react-lite';
import { lazy, Suspense } from 'react';
import type { RulesetScope, RulesetTarget } from '../../api/rulesets';
import { useQuery } from '../../router';
import type { ID, Repo } from '../../sync/models';
import { TabNav } from '../../ui/Tabs';
import { ListSkeleton } from '../repo-settings/shared';
import { RulesetList } from './RulesetList';
import styles from './Rulesets.module.css';

const RulesetEditor = lazy(() => import('./RulesetEditor'));
const RuleInsights = lazy(() => import('./RuleInsights'));

export interface RulesetsHost {
  scope: RulesetScope;
  /** Path of the rules page (`…/settings/rules`). */
  base: string;
  /** Organization owning the rulesets (or the repository), when any. */
  orgId: ID | null;
  /** The repository of repository rulesets. */
  repo?: Repo;
  readOnly?: boolean;
}

export default observer(function RulesetsSection({ host, rest }: { host: RulesetsHost; rest: string[] }) {
  const query = useQuery();
  const first = rest[0] ?? '';
  const tab = first === 'insights' ? 'insights' : 'rulesets';
  const tabs = (
    <TabNav
      className={styles.tabs}
      aria-label="Rules"
      current={tab}
      items={[
        { id: 'rulesets', label: 'Rulesets', href: host.base },
        { id: 'insights', label: 'Insights', href: `${host.base}/insights` },
      ]}
    />
  );
  let body;
  if (first === 'insights') body = <RuleInsights host={host} />;
  else if (first === 'new') {
    const t = query.get('target');
    const target: RulesetTarget = t === 'tag' || t === 'push' ? t : 'branch';
    body = <RulesetEditor key={`new:${target}:${query.get('import') ?? ''}`} host={host} id={null} target={target} imported={query.get('import') === '1'} />;
  } else if (/^\d+$/.test(first)) body = <RulesetEditor key={first} host={host} id={Number(first)} target="branch" imported={false} />;
  else body = <RulesetList host={host} />;
  return (
    <>
      {first === '' || first === 'insights' ? tabs : null}
      <Suspense fallback={<ListSkeleton rows={6} />}>{body}</Suspense>
    </>
  );
});
