import { useReducer } from 'react';
import { invalidate, useResource } from '../../api/cache';
import { listOwnerPackages, packageHref, packageKeys, type PackageSummary, type PackageVisibility } from '../../api/packages';
import { formatBytes, plural } from '../../components/admin/format';
import { Pill } from '../../components/settings/kit';
import { Link } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, LockIcon, PackageIcon, RepoIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Packages.module.css';

export function VisibilityPill({ visibility }: { visibility: PackageVisibility }) {
  return (
    <Pill tone={visibility === 'public' ? 'neutral' : 'warning'}>
      {visibility !== 'public' && <LockIcon size={12} />}
      {visibility === 'public' ? 'Public' : visibility === 'private' ? 'Private' : 'Internal'}
    </Pill>
  );
}

/** "How to publish" commands shown when an owner has no packages. */
export function PushHint({ registry, owner }: { registry: string; owner: string }) {
  const image = `${registry}/${owner.toLowerCase()}/IMAGE_NAME`;
  return (
    <div className={styles.hint}>
      <span className={styles.small}>Push a container image with Docker:</span>
      <code className={styles.code}>
        {`docker login ${registry} -u USERNAME\ndocker tag IMAGE_ID ${image}:latest\ndocker push ${image}:latest`}
      </code>
    </div>
  );
}

function Row({ p }: { p: PackageSummary }) {
  const tags = p.latest?.tags ?? [];
  return (
    <div className={styles.row} role="listitem">
      <PackageIcon size={16} className={styles.rowIcon} />
      <div className={styles.rowBody}>
        <div className={styles.rowTitle}>
          <Link to={packageHref(p)} className={styles.rowName}>
            {p.name}
          </Link>
          <VisibilityPill visibility={p.visibility} />
        </div>
        <div className={styles.meta}>
          {p.repository && (
            <Link to={`/${p.repository.full_name}`}>
              <RepoIcon size={14} />
              {p.repository.full_name}
            </Link>
          )}
          {tags.length > 0 && (
            <span className={styles.tags} aria-label="Latest tags">
              {tags.slice(0, 4).map((t) => (
                <span key={t} className={styles.tag}>
                  {t}
                </span>
              ))}
              {tags.length > 4 && <span>+{tags.length - 4}</span>}
            </span>
          )}
          <span>{plural(p.version_count, 'version')}</span>
        </div>
      </div>
      <div className={styles.rowSide}>
        <span>
          Updated <RelativeTime date={p.updated_at} />
        </span>
        <span>{formatBytes(p.size)}</span>
      </div>
    </div>
  );
}

/** Packages of a user or organization (profile tab and `/orgs|users/:owner/packages`). */
export function PackageList({ owner }: { owner: string }) {
  const [, retry] = useReducer((x: number) => x + 1, 0);
  const res = useResource(packageKeys.owner(owner), () => listOwnerPackages(owner));
  const data = res.data;
  if (!data) {
    if (res.error) {
      return (
        <EmptyState
          icon={AlertIcon}
          title="Could not load packages"
          action={
            <Button
              onClick={() => {
                invalidate(packageKeys.owner(owner));
                retry();
              }}
            >
              Retry
            </Button>
          }
        >
          {res.error instanceof Error ? res.error.message : null}
        </EmptyState>
      );
    }
    return (
      <div className={styles.rows} aria-busy="true" aria-label="Loading packages">
        {[0, 1, 2].map((i) => (
          <div key={i} className={styles.row}>
            <div className={styles.rowBody}>
              <Skeleton width="30%" height={18} />
              <Skeleton width="60%" />
            </div>
          </div>
        ))}
      </div>
    );
  }
  return (
    <section aria-label="Packages">
      <div className={styles.listHead}>
        <h2 className={styles.listTitle}>Packages</h2>
        {data.packages.length > 0 && <span className={`${styles.muted} ${styles.small}`}>{plural(data.packages.length, 'package')}</span>}
      </div>
      {data.packages.length === 0 ? (
        <EmptyState icon={PackageIcon} title={`${data.owner.login} has no packages yet`}>
          <PushHint registry={data.registry} owner={data.owner.login} />
        </EmptyState>
      ) : (
        <div className={styles.rows} role="list" aria-label="Packages">
          {data.packages.map((p) => (
            <Row key={p.id} p={p} />
          ))}
        </div>
      )}
    </section>
  );
}
