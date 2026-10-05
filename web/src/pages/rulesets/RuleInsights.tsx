import { observer } from 'mobx-react-lite';
import { useId, useState, type ReactNode } from 'react';
import { useResource } from '../../api/cache';
import { getRuleSuite, ruleSuitesPath, scopeKey, type RuleSuite, type SuiteFilters, type SuiteResult } from '../../api/rulesets';
import { Drawer, ErrorState, KeyValue } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { PageHeader } from '../../components/settings/kit';
import { setQuery, useQuery } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, GraphIcon, SkipIcon, XCircleIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { RULE_DEF, isRuleType } from './model';
import styles from './Rulesets.module.css';
import type { RulesetsHost } from './RulesetsSection';

const RESULT_LABEL: Record<SuiteResult, string> = { pass: 'Pass', fail: 'Fail', bypass: 'Bypass' };
const RESULT_ICON = { pass: CheckCircleIcon, fail: XCircleIcon, bypass: SkipIcon };

export function ResultLabel({ result }: { result: SuiteResult | 'pass' | 'fail' }) {
  const I = RESULT_ICON[result];
  return (
    <span className={styles.result} data-result={result}>
      <I size={14} />
      {RESULT_LABEL[result]}
    </span>
  );
}

const shortRef = (ref: string) => ref.replace(/^refs\/(heads|tags)\//, '');
const ruleTitle = (t: string) => (isRuleType(t) ? RULE_DEF[t].title : t);

/** Rule insights: recorded ruleset evaluations, filterable by ref, actor, result, time and repository. */
export default observer(function RuleInsights({ host }: { host: RulesetsHost }) {
  const { scope } = host;
  const q = useQuery();
  const filters: SuiteFilters = {
    ref: q.get('ref') ?? undefined,
    actor_name: q.get('actor') ?? undefined,
    rule_suite_result: (q.get('result') as SuiteFilters['rule_suite_result']) ?? undefined,
    time_period: (q.get('period') as SuiteFilters['time_period']) ?? 'day',
    repository_name: q.get('repo') ?? undefined,
  };
  const list = usePagedList<RuleSuite>(ruleSuitesPath(scope, filters));
  const selected = Number(q.get('suite')) || null;
  const ids = { ref: useId(), actor: useId(), result: useId(), period: useId(), repo: useId() };
  // Text filters apply on Enter / blur, not on every keystroke.
  const [ref, setRef] = useState(filters.ref ?? '');
  const [actor, setActor] = useState(filters.actor_name ?? '');
  const [repoName, setRepoName] = useState(filters.repository_name ?? '');

  const applyText = () => setQuery({ ref: ref.trim() || null, actor: actor.trim() || null, repo: repoName.trim() || null });

  return (
    <>
      <PageHeader title="Rule insights" description="Every push and merge evaluated against rulesets, including rulesets in evaluate mode." />
      <div className={styles.filters} role="search">
        <Field label="Branch or tag" htmlFor={ids.ref}>
          <Input id={ids.ref} value={ref} placeholder="All refs" onChange={(e) => setRef(e.target.value)} onBlur={applyText} onKeyDown={(e) => e.key === 'Enter' && applyText()} />
        </Field>
        <Field label="Actor" htmlFor={ids.actor}>
          <Input id={ids.actor} value={actor} placeholder="All actors" onChange={(e) => setActor(e.target.value)} onBlur={applyText} onKeyDown={(e) => e.key === 'Enter' && applyText()} />
        </Field>
        {scope.kind === 'org' && (
          <Field label="Repository" htmlFor={ids.repo}>
            <Input id={ids.repo} value={repoName} placeholder="All repositories" onChange={(e) => setRepoName(e.target.value)} onBlur={applyText} onKeyDown={(e) => e.key === 'Enter' && applyText()} />
          </Field>
        )}
        <Field label="Result" htmlFor={ids.result}>
          <Select id={ids.result} value={filters.rule_suite_result ?? 'all'} onChange={(e) => setQuery({ result: e.target.value === 'all' ? null : e.target.value })}>
            <option value="all">All results</option>
            <option value="pass">Pass</option>
            <option value="fail">Fail</option>
            <option value="bypass">Bypass</option>
          </Select>
        </Field>
        <Field label="Time period" htmlFor={ids.period}>
          <Select id={ids.period} value={filters.time_period} onChange={(e) => setQuery({ period: e.target.value === 'day' ? null : e.target.value })}>
            <option value="hour">Last hour</option>
            <option value="day">Last 24 hours</option>
            <option value="week">Last 7 days</option>
            <option value="month">Last 30 days</option>
          </Select>
        </Field>
      </div>
      {list.error ? (
        <ErrorState error={list.error} onRetry={() => void list.reload()} title="Could not load rule insights" />
      ) : list.items.length === 0 && list.loading ? (
        <Skeleton height={120} />
      ) : list.items.length === 0 ? (
        <EmptyState icon={GraphIcon} title="No rule evaluations">
          Nothing was evaluated against rulesets for these filters. Try a longer time period.
        </EmptyState>
      ) : (
        <>
          <table className={styles.table} aria-label="Rule suites">
            <thead>
              <tr>
                <th>Result</th>
                <th>Ref</th>
                {scope.kind === 'org' && <th>Repository</th>}
                <th>Actor</th>
                <th>Evaluated</th>
              </tr>
            </thead>
            <tbody>
              {list.items.map((s) => (
                <tr
                  key={s.id}
                  className={styles.clickable}
                  tabIndex={0}
                  onClick={() => setQuery({ suite: String(s.id) })}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') setQuery({ suite: String(s.id) });
                  }}
                >
                  <td>
                    <ResultLabel result={s.result} />
                    {s.evaluation_result && s.evaluation_result !== s.result && <span className={styles.muted}> (evaluate: {RESULT_LABEL[s.evaluation_result]})</span>}
                  </td>
                  <td className={styles.mono}>{shortRef(s.ref)}</td>
                  {scope.kind === 'org' && <td>{s.repository_name}</td>}
                  <td>{s.actor_name ?? '—'}</td>
                  <td>
                    <RelativeTime date={s.pushed_at} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {list.next && (
            <Button size="sm" loading={list.loading} onClick={() => void list.loadMore()} style={{ marginTop: 12 }}>
              Load more
            </Button>
          )}
        </>
      )}
      <Drawer open={!!selected} onClose={() => setQuery({ suite: null })} title="Rule evaluation">
        {selected && <SuiteDetail host={host} id={selected} />}
      </Drawer>
    </>
  );
});

function SuiteDetail({ host, id }: { host: RulesetsHost; id: number }) {
  const res = useResource<RuleSuite>(`${scopeKey(host.scope, 'suite')}${id}`, () => getRuleSuite(host.scope, id), { immutable: true });
  if (res.error) return <ErrorState error={res.error} title="Could not load this evaluation" />;
  const s = res.data;
  if (!s) return <Skeleton height={160} />;
  const evals = s.rule_evaluations ?? [];
  return (
    <div className={styles.rules}>
      <KeyValue
        items={[
          ['Result', <ResultLabel key="r" result={s.result} />],
          ...(s.evaluation_result ? ([['Evaluate-mode result', <ResultLabel key="e" result={s.evaluation_result} />]] as [string, ReactNode][]) : []),
          ['Ref', <span key="ref" className={styles.mono}>{s.ref}</span>],
          ['Repository', s.repository_name],
          ['Actor', s.actor_name ?? '—'],
          ['Before', <span key="b" className={styles.mono}>{s.before_sha.slice(0, 12)}</span>],
          ['After', <span key="a" className={styles.mono}>{s.after_sha.slice(0, 12)}</span>],
          ['Evaluated', <RelativeTime key="t" date={s.pushed_at} />],
        ]}
      />
      {evals.length === 0 ? (
        <p className={styles.muted}>No rules applied to this ref update.</p>
      ) : (
        <table className={styles.table} aria-label="Rule evaluations">
          <thead>
            <tr>
              <th>Rule</th>
              <th>Ruleset</th>
              <th>Result</th>
            </tr>
          </thead>
          <tbody>
            {evals.map((e, i) => (
              <tr key={i}>
                <td>
                  {ruleTitle(e.rule_type)}
                  {e.details && <div className={styles.muted}>{e.details}</div>}
                </td>
                <td>
                  {e.rule_source.name ?? '—'}
                  {e.enforcement !== 'active' && <div className={styles.muted}>{e.enforcement}</div>}
                </td>
                <td>
                  <ResultLabel result={e.result} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
