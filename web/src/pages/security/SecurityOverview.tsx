import { observer } from 'mobx-react-lite';
import { alertsPath } from '../../api/secretScanning';
import { usePagedList } from '../../components/admin/usePagedList';
import { Link, useParams } from '../../router';
import { Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, CircleSlashIcon, InfoIcon, KeyAsteriskIcon } from '../../ui/icons';
import styles from './Security.module.css';
import { SecurityFrame, isDisabledError, useCanAdmin, useSecretSettings } from './shared';

/** `/:owner/:repo/security`: overview cards (secret scanning now; code scanning and advisories come with P66). */
export default observer(function SecurityOverview() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>();
  return (
    <SecurityFrame owner={owner} repo={repo}>
      <div className={styles.header}>
        <h1 className={styles.title}>Security overview</h1>
      </div>
      <div className={styles.cards}>
        <SecretScanningCard owner={owner} repo={repo} />
      </div>
      <p className={`${styles.muted} ${styles.small}`}>
        <InfoIcon size={14} /> Code scanning, Dependabot alerts and security advisories will appear here in a later release.
      </p>
    </SecurityFrame>
  );
});

const SecretScanningCard = observer(function SecretScanningCard({ owner, repo }: { owner: string; repo: string }) {
  const settings = useSecretSettings(owner, repo);
  const canAdmin = useCanAdmin(owner, repo);
  const enabled = settings.data?.secret_scanning ?? false;
  // per_page=1: the `last` link's page number is the open count.
  const open = usePagedList(enabled ? alertsPath(owner, repo, { state: 'open', per_page: 1 }) : null);
  const base = `/${owner}/${repo}`;
  const count = open.done ? open.items.length : open.totalUpperBound;
  const forbidden = !!open.error && !isDisabledError(open.error);

  const Status = ({ on, children }: { on: boolean; children: string }) => (
    <li>
      {on ? <CheckCircleIcon size={14} className={styles.on} /> : <CircleSlashIcon size={14} className={styles.off} />}
      {children} {on ? 'enabled' : 'disabled'}
    </li>
  );

  return (
    <section className={styles.card} aria-labelledby="ss-card-title">
      <div className={styles.cardHead}>
        <KeyAsteriskIcon size={16} />
        <span id="ss-card-title">Secret scanning</span>
      </div>
      {settings.error ? (
        <p className={styles.muted}>Could not load secret scanning settings.</p>
      ) : !settings.data ? (
        <Skeleton width="60%" />
      ) : !settings.data.available ? (
        <p className={styles.muted}>Secret scanning is not available on this instance.</p>
      ) : (
        <>
          {enabled &&
            !forbidden &&
            (count == null ? (
              <Skeleton width={60} height={28} />
            ) : (
              <Link to={`${base}/security/secret-scanning`} className={styles.bigCount} aria-label={`${count} open secret scanning alerts`}>
                {count} <span className={`${styles.muted} ${styles.small}`}>open {count === 1 ? 'alert' : 'alerts'}</span>
              </Link>
            ))}
          <ul className={styles.statusList}>
            <Status on={settings.data.secret_scanning}>Secret scanning</Status>
            <Status on={settings.data.push_protection}>Push protection</Status>
          </ul>
          <p className={`${styles.muted} ${styles.small}`}>
            {enabled ? 'Secrets such as API keys and tokens pushed to this repository raise alerts.' : 'Turn on secret scanning to get alerts for secrets pushed to this repository.'}
          </p>
          <p className={styles.small}>
            {enabled && <Link to={`${base}/security/secret-scanning`}>View alerts</Link>}
            {enabled && canAdmin && ' · '}
            {canAdmin && <Link to={`${base}/settings/security_analysis`}>Configure</Link>}
          </p>
        </>
      )}
    </section>
  );
});
