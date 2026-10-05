import { observer } from 'mobx-react-lite';
import styles from '../../components/admin/admin.module.css';
import { PageHeader, Panel, errorMessage } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { orgAlertsPath, type SecretScanningAlert } from '../../api/secretScanning';
import { Link, useParams } from '../../router';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { ShieldIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { CustomPatternsPanel } from '../security/CustomPatternsPanel';
import sec from '../security/Security.module.css';
import { BypassBadge } from '../security/shared';
import { OwnerRequired, useOrgAccess } from './common';

/** `/organizations/:org/settings/security_analysis`: org-wide open secret scanning alerts and custom patterns (owners only). */
export default observer(function OrgSecretScanningPage() {
  const { org = '' } = useParams<{ org: string }>();
  const access = useOrgAccess(org);
  return (
    <div className={styles.page}>
      <PageHeader title="Secret scanning" description={`Open alerts across the repositories of ${org}, and patterns every repository is scanned for.`} />
      {access.loading ? (
        <Skeleton height={160} />
      ) : !access.isOwner ? (
        <OwnerRequired org={org} what="manage secret scanning" />
      ) : (
        <div className={styles.stack}>
          <OrgAlerts org={org} />
          <CustomPatternsPanel scope={{ kind: 'org', org }} />
        </div>
      )}
    </div>
  );
});

function OrgAlerts({ org }: { org: string }) {
  const list = usePagedList<SecretScanningAlert>(orgAlertsPath(org));
  return (
    <Panel title="Open alerts" padded={false}>
      {list.error ? (
        <div className={sec.empty}>{errorMessage(list.error)}</div>
      ) : list.items.length === 0 && list.loading ? (
        <div className={sec.empty}>
          <Skeleton width="50%" />
        </div>
      ) : list.items.length === 0 ? (
        <div className={`${sec.empty} ${sec.muted}`}>No open secret scanning alerts in this organization.</div>
      ) : (
        <>
          <ul className={sec.rows} aria-label="Organization secret scanning alerts">
            {list.items.map((a) => {
              const r = a.repository;
              const owner = r?.owner.login ?? org;
              const name = r?.name ?? '';
              const loc = a.first_location_detected;
              return (
                <li key={`${r?.full_name}#${a.number}`} className={sec.row}>
                  <ShieldIcon size={16} className={`${sec.rowIcon} ${sec.iconOpen}`} />
                  <div className={sec.rowMain}>
                    <Link to={`/${owner}/${name}/security/secret-scanning/${a.number}`} className={sec.rowTitle}>
                      {a.secret_type_display_name}
                      {a.push_protection_bypassed && <BypassBadge />}
                    </Link>
                    <span className={sec.rowMeta}>
                      <Link to={`/${owner}/${name}/security/secret-scanning`}>{r?.full_name ?? `${owner}/${name}`}</Link> #{a.number} opened <RelativeTime date={a.created_at} />
                      {loc && (
                        <>
                          {' · '}
                          <span className={sec.mono}>
                            {loc.path}:{loc.start_line}
                          </span>
                        </>
                      )}
                    </span>
                  </div>
                </li>
              );
            })}
          </ul>
          {list.next && (
            <div className={sec.more}>
              <Button size="sm" loading={list.loading} onClick={() => void list.loadMore()}>
                Load more
              </Button>
            </div>
          )}
        </>
      )}
    </Panel>
  );
}
