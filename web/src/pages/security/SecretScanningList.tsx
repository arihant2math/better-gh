import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { alertsPath, listPatterns, resolutionLabel, ssKeys, type SecretScanningAlert } from '../../api/secretScanning';
import { errorMessage } from '../../components/settings/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { Link, setQuery, useParams, useQuery } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, KeyAsteriskIcon, ShieldIcon } from '../../ui/icons';
import { Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Security.module.css';
import { BypassBadge, SecurityFrame, isDisabledError, useCanAdmin } from './shared';

/** `/:owner/:repo/security/secret-scanning`: alerts with Open/Closed and secret type filters. */
export default observer(function SecretScanningList() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>();
  const query = useQuery();
  const state = query.get('state') === 'resolved' || query.get('state') === 'closed' ? 'resolved' : 'open';
  const type = query.get('secret_type') ?? '';
  const canAdmin = useCanAdmin(owner, repo);
  const list = usePagedList<SecretScanningAlert>(alertsPath(owner, repo, { state, secret_type: type || undefined }));
  const other = usePagedList<SecretScanningAlert>(alertsPath(owner, repo, { state: state === 'open' ? 'resolved' : 'open', secret_type: type || undefined, per_page: 1 }));
  const patterns = useResource(ssKeys.patterns(), listPatterns, { ttlMs: 10 * 60_000 });
  const counts = {
    [state]: list.done ? list.items.length : list.totalUpperBound,
    [state === 'open' ? 'resolved' : 'open']: other.done ? other.items.length : other.totalUpperBound,
  } as Record<'open' | 'resolved', number | null>;
  const disabled = isDisabledError(list.error);
  const base = `/${owner}/${repo}`;

  return (
    <SecurityFrame owner={owner} repo={repo}>
      <div className={styles.header}>
        <h1 className={styles.title}>Secret scanning alerts</h1>
      </div>
      {disabled ? (
        <div className={styles.box}>
          <div className={styles.empty}>
            <EmptyState
              icon={KeyAsteriskIcon}
              title="Secret scanning is disabled"
              action={
                canAdmin ? (
                  <Link to={`${base}/settings/security_analysis`}>Enable secret scanning in settings</Link>
                ) : undefined
              }
            >
              {canAdmin ? 'Turn on secret scanning to detect API keys, tokens and other secrets pushed to this repository.' : 'A repository administrator can turn on secret scanning in the repository settings.'}
            </EmptyState>
          </div>
        </div>
      ) : (
        <div className={styles.box}>
          <div className={styles.boxHeader}>
            <div className={styles.stateToggle}>
              {(['open', 'resolved'] as const).map((s) => (
                <Link key={s} to={`${base}/security/secret-scanning?${new URLSearchParams({ ...(s === 'resolved' ? { state: 'resolved' } : {}), ...(type ? { secret_type: type } : {}) })}`} className={styles.stateLink} aria-current={s === state ? 'true' : undefined}>
                  {s === 'open' ? <ShieldIcon size={16} /> : <CheckCircleIcon size={16} />}
                  {counts[s] ?? ''} {s === 'open' ? 'Open' : 'Closed'}
                </Link>
              ))}
            </div>
            <Select id="ss-type" className={styles.filter} value={type} onChange={(e) => setQuery({ secret_type: e.target.value || undefined })} aria-label="Secret type">
              <option value="">All secret types</option>
              {(patterns.data ?? []).map((p) => (
                <option key={p.secret_type} value={p.secret_type}>
                  {p.display_name}
                </option>
              ))}
              {type && !patterns.data?.some((p) => p.secret_type === type) && <option value={type}>{type}</option>}
            </Select>
          </div>
          {list.error ? (
            <div className={styles.empty}>
              <EmptyState icon={KeyAsteriskIcon} title="Could not load alerts">
                {errorMessage(list.error)}
              </EmptyState>
            </div>
          ) : list.items.length === 0 && list.loading ? (
            <ul className={styles.rows} aria-busy="true">
              {[0, 1, 2].map((i) => (
                <li key={i} className={styles.row}>
                  <Skeleton width={16} height={16} />
                  <Skeleton width={`${50 - i * 10}%`} />
                </li>
              ))}
            </ul>
          ) : list.items.length === 0 ? (
            <div className={styles.empty}>
              <EmptyState icon={ShieldIcon} title={state === 'open' ? 'No open alerts' : 'No closed alerts'}>
                {state === 'open' ? 'No secrets have been detected in this repository' + (type ? ' for this secret type.' : '.') : 'Closed alerts appear here.'}
              </EmptyState>
            </div>
          ) : (
            <>
              <ul className={styles.rows} aria-label="Secret scanning alerts">
                {list.items.map((a) => (
                  <AlertRow key={a.number} alert={a} base={base} />
                ))}
              </ul>
              {list.next && (
                <div className={styles.more}>
                  <Button size="sm" loading={list.loading} onClick={() => void list.loadMore()}>
                    Load more
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
      )}
    </SecurityFrame>
  );
});

function AlertRow({ alert: a, base }: { alert: SecretScanningAlert; base: string }) {
  const loc = a.first_location_detected;
  return (
    <li className={styles.row}>
      {a.state === 'open' ? <ShieldIcon size={16} className={`${styles.rowIcon} ${styles.iconOpen}`} /> : <CheckCircleIcon size={16} className={`${styles.rowIcon} ${styles.iconClosed}`} />}
      <div className={styles.rowMain}>
        <Link to={`${base}/security/secret-scanning/${a.number}`} className={styles.rowTitle}>
          {a.secret_type_display_name}
          {a.push_protection_bypassed && <BypassBadge />}
        </Link>
        <span className={styles.rowMeta}>
          #{a.number} {a.state === 'open' ? 'opened' : `closed as ${resolutionLabel(a.resolution).toLowerCase()}`} <RelativeTime date={a.state === 'open' ? a.created_at : (a.resolved_at ?? a.created_at)} />
          {loc && (
            <>
              {' · '}
              <span className={styles.mono}>
                {loc.path}:{loc.start_line}
              </span>
              {a.has_more_locations && ' and more'}
            </>
          )}
        </span>
      </div>
    </li>
  );
}
