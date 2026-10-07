import { observer } from 'mobx-react-lite';
import { useReducer, useState } from 'react';
import { invalidate, mutate, refresh, useResource } from '../../api/cache';
import {
  defaultTag,
  deletePackage,
  deletePackageVersion,
  getPackage,
  packageKeys,
  packagesHref,
  pullCommand,
  shortDigest,
  updatePackage,
  versionTags,
  type PackageDetail,
  type PackagePatch,
  type VersionSummary,
} from '../../api/packages';
import { NotFound } from '../../app/NotFound';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import { errorMessage, Panel, useConfirm } from '../../components/admin/kit';
import { formatBytes, formatDateTime, plural } from '../../components/admin/format';
import { CopyButton } from '../../components/settings/kit';
import { Link, navigate, useParams } from '../../router';
import { reposForOwner } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, PackageIcon, RepoIcon, TagIcon, TrashIcon } from '../../ui/icons';
import { Input, Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { PushHint, VisibilityPill } from './PackageList';
import styles from './Packages.module.css';

type Ref = { tag: string } | { digest: string };

/** `/users|orgs/:owner/packages/:type/package/*` — a container package. */
export default observer(function PackagePage() {
  const params = useParams<{ owner: string; type: string; '*': string }>();
  const { owner, type } = params;
  const name = params['*'];
  const key = packageKeys.detail(owner, type, name);
  const loader = () => getPackage(owner, type, name);
  const [, retry] = useReducer((x: number) => x + 1, 0);
  const res = useResource(key, loader);
  const [ref, setRef] = useState<Ref | null>(null);
  const [pendingDelete, setPendingDelete] = useState<VersionSummary | null>(null);
  const confirm = useConfirm();

  const d = res.data;
  if (!d) {
    if ((res.error as { status?: number } | undefined)?.status === 404) return <NotFound what="package" />;
    if (res.error) {
      return (
        <div className={styles.page}>
          <EmptyState
            icon={AlertIcon}
            title="Could not load this package"
            action={
              <Button
                onClick={() => {
                  invalidate(key);
                  retry();
                }}
              >
                Retry
              </Button>
            }
          >
            {errorMessage(res.error)}
          </EmptyState>
        </div>
      );
    }
    return (
      <div className={styles.page} aria-busy="true" aria-label="Loading package">
        <Skeleton width="40%" height={28} />
        <div style={{ height: 24 }} />
        <Skeleton height={96} />
        <div style={{ height: 24 }} />
        <Skeleton height={200} />
      </div>
    );
  }

  const pkg = d.package;
  const listsChanged = () => {
    invalidate(packageKeys.owner(pkg.owner.login));
    invalidate('packages:repo:');
  };

  // Effective install reference: the picked tag/digest if it still exists, else the default tag.
  const allTags = [...new Set(d.versions.flatMap(versionTags))];
  const picked = ref && ('tag' in ref ? allTags.includes(ref.tag) : d.versions.some((v) => v.digest === ref.digest)) ? ref : null;
  const fallbackTag = defaultTag(d.versions);
  const current: Ref | null = picked ?? (fallbackTag ? { tag: fallbackTag } : d.versions[0] ? { digest: d.versions[0].digest } : null);
  const command = current ? pullCommand(d.registry, pkg.owner.login, pkg.name, current) : null;
  const isPicked = (v: VersionSummary, tag?: string) => !!current && (tag ? 'tag' in current && current.tag === tag : 'digest' in current && current.digest === v.digest);

  const removeVersion = async (v: VersionSummary) => {
    mutate<PackageDetail>(key, (prev) => (prev ? { ...prev, versions: prev.versions.filter((x) => x.id !== v.id) } : d));
    try {
      await deletePackageVersion(pkg, v.id);
      toast({ kind: 'success', title: 'Version deleted' });
      listsChanged();
    } catch (err) {
      toast({ kind: 'error', title: 'Could not delete this version', description: errorMessage(err) });
    }
    void refresh(key, loader).catch(() => undefined);
  };

  const patch = async (body: PackagePatch, success: string) => {
    const next = await updatePackage(owner, type, pkg.name, body);
    mutate<PackageDetail>(key, () => next);
    listsChanged();
    toast({ kind: 'success', title: success });
  };

  const latest = d.versions[0];

  return (
    <div className={styles.page}>
      <nav className={styles.crumbs} aria-label="Breadcrumb">
        <Avatar user={{ login: pkg.owner.login, avatarUrl: pkg.owner.avatar_url, name: null }} size={20} square={pkg.owner.type === 'Organization'} />
        <Link to={`/${pkg.owner.login}`}>{pkg.owner.login}</Link>
        <span aria-hidden>/</span>
        <Link to={packagesHref(pkg.owner)}>Packages</Link>
      </nav>
      <header className={styles.header}>
        <div>
          <h1 className={styles.title}>
            <PackageIcon size={24} />
            {pkg.name}
            <VisibilityPill visibility={pkg.visibility} />
          </h1>
          <div className={`${styles.meta} ${styles.headerMeta}`}>
            {pkg.repository ? (
              <Link to={`/${pkg.repository.full_name}`}>
                <RepoIcon size={14} />
                {pkg.repository.full_name}
              </Link>
            ) : (
              <span>Not linked to a repository</span>
            )}
            {latest && (
              <span>
                Published <RelativeTime date={latest.created_at} />
              </span>
            )}
            <span>{plural(d.versions.length, 'version')}</span>
            <span>{formatBytes(d.size)} total</span>
          </div>
        </div>
      </header>

      <div className={styles.layout}>
        <div className={styles.main}>
          <section className={styles.section} aria-labelledby="pkg-install-h">
            <div className={styles.install}>
              <div className={styles.installHead}>
                <h2 id="pkg-install-h" className={styles.sectionTitle} style={{ margin: 0 }}>
                  Install from the command line
                </h2>
                {allTags.length > 0 && (
                  <label>
                    <TagIcon size={14} />
                    Tag
                    <Select
                      className={styles.tagSelect}
                      aria-label="Tag"
                      value={current && 'tag' in current ? current.tag : ''}
                      onChange={(e) => setRef({ tag: e.target.value })}
                    >
                      {current && 'digest' in current && <option value="">@{shortDigest(current.digest)}</option>}
                      {allTags.map((t) => (
                        <option key={t} value={t}>
                          {t}
                        </option>
                      ))}
                    </Select>
                  </label>
                )}
              </div>
              {command ? (
                <div className={styles.command}>
                  <code aria-label="Pull command">{command}</code>
                  <CopyButton value={command} />
                </div>
              ) : (
                <PushHint registry={d.registry} owner={pkg.owner.login} />
              )}
            </div>
          </section>

          <section className={styles.section} aria-labelledby="pkg-versions-h">
            <h2 id="pkg-versions-h" className={styles.sectionTitle}>
              Versions
            </h2>
            {d.versions.length === 0 ? (
              <EmptyState icon={PackageIcon} title="No versions">
                This package has no versions.
              </EmptyState>
            ) : (
              <div className={styles.tableWrap}>
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th>Tags</th>
                      <th>Digest</th>
                      <th>Platforms</th>
                      <th className={styles.num}>Size</th>
                      <th>Published</th>
                      {d.viewer_can_admin && (
                        <th className={styles.actionsCell}>
                          <span className="visually-hidden">Actions</span>
                        </th>
                      )}
                    </tr>
                  </thead>
                  <tbody>
                    {d.versions.map((v) => {
                      const tags = versionTags(v);
                      return (
                        <tr key={v.id}>
                          <td>
                            {tags.length ? (
                              <span className={styles.tags}>
                                {tags.map((t) => (
                                  <button key={t} type="button" className={styles.tag} aria-pressed={isPicked(v, t)} title={`Show the pull command for ${t}`} onClick={() => setRef({ tag: t })}>
                                    {t}
                                  </button>
                                ))}
                              </span>
                            ) : (
                              <span className={styles.muted}>untagged</span>
                            )}
                          </td>
                          <td className={styles.nowrap}>
                            <button type="button" className={styles.digest} title={`${v.digest} — show the pull command for this digest`} aria-pressed={isPicked(v)} onClick={() => setRef({ digest: v.digest })}>
                              {shortDigest(v.digest)}
                            </button>
                          </td>
                          <td>
                            {v.platforms.length ? (
                              <span className={styles.platforms}>
                                {v.platforms.map((pl) => (
                                  <span key={pl} className={styles.platform}>
                                    {pl}
                                  </span>
                                ))}
                              </span>
                            ) : (
                              <span className={styles.muted}>—</span>
                            )}
                          </td>
                          <td className={styles.num}>{formatBytes(v.size)}</td>
                          <td className={styles.nowrap} title={formatDateTime(v.created_at)}>
                            <RelativeTime date={v.created_at} />
                          </td>
                          {d.viewer_can_admin && (
                            <td className={styles.actionsCell}>
                              <IconButton icon={TrashIcon} label={`Delete version ${tags[0] ?? shortDigest(v.digest)}`} size="sm" onClick={() => setPendingDelete(v)} />
                            </td>
                          )}
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {d.viewer_can_admin && (
            <Settings
              detail={d}
              onPatch={patch}
              onDelete={() =>
                confirm({
                  title: `Delete ${pkg.name}?`,
                  danger: true,
                  confirmLabel: 'I understand, delete this package',
                  confirmText: pkg.name,
                  body: (
                    <p>
                      This permanently deletes the package and all {plural(d.versions.length, 'version')}. Anyone pulling <code>{pkg.name}</code> will get an error.
                    </p>
                  ),
                  onConfirm: async () => {
                    await deletePackage(pkg);
                    listsChanged();
                    invalidate(key);
                    toast({ kind: 'success', title: `Deleted ${pkg.name}` });
                    navigate(packagesHref(pkg.owner));
                  },
                })
              }
              onVisibility={(visibility) =>
                confirm({
                  title: `Make ${pkg.name} ${visibility}?`,
                  confirmLabel: `Make ${visibility}`,
                  danger: visibility === 'public',
                  body:
                    visibility === 'public' ? (
                      <p>Anyone will be able to see and pull this package.</p>
                    ) : (
                      <p>Only people with access to {pkg.owner.login} will be able to see and pull this package.</p>
                    ),
                  onConfirm: () => patch({ visibility }, `${pkg.name} is now ${visibility}`),
                })
              }
            />
          )}
        </div>

        <aside className={styles.side} aria-label="Package details">
          <div>
            <h2 className={styles.sideTitle}>Details</h2>
            <dl className={styles.facts}>
              <div>
                <dt>Owner</dt>
                <dd>
                  <Link to={`/${pkg.owner.login}`}>{pkg.owner.login}</Link>
                </dd>
              </div>
              <div>
                <dt>Repository</dt>
                <dd>{pkg.repository ? <Link to={`/${pkg.repository.full_name}`}>{pkg.repository.name}</Link> : <span className={styles.muted}>None</span>}</dd>
              </div>
              <div>
                <dt>Visibility</dt>
                <dd>{pkg.visibility}</dd>
              </div>
              <div>
                <dt>Versions</dt>
                <dd>{pkg.version_count}</dd>
              </div>
              <div>
                <dt>Total size</dt>
                <dd>{formatBytes(d.size)}</dd>
              </div>
              <div>
                <dt>Created</dt>
                <dd title={formatDateTime(pkg.created_at)}>
                  <RelativeTime date={pkg.created_at} />
                </dd>
              </div>
              <div>
                <dt>Updated</dt>
                <dd title={formatDateTime(pkg.updated_at)}>
                  <RelativeTime date={pkg.updated_at} />
                </dd>
              </div>
            </dl>
          </div>
          {allTags.length > 0 && (
            <div>
              <h2 className={styles.sideTitle}>Tags</h2>
              <span className={styles.tags}>
                {allTags.map((t) => (
                  <button key={t} type="button" className={styles.tag} aria-pressed={!!current && 'tag' in current && current.tag === t} onClick={() => setRef({ tag: t })}>
                    {t}
                  </button>
                ))}
              </span>
            </div>
          )}
        </aside>
      </div>

      <ConfirmDialog
        open={!!pendingDelete}
        onClose={() => setPendingDelete(null)}
        title="Delete this version?"
        confirmLabel="Delete version"
        onConfirm={() => {
          if (pendingDelete) void removeVersion(pendingDelete);
        }}
      >
        {pendingDelete && (
          <p>
            Version <code>{shortDigest(pendingDelete.digest)}</code>
            {versionTags(pendingDelete).length > 0 && <> (tagged {versionTags(pendingDelete).join(', ')})</>} will be deleted. Pulls by this tag or digest will fail.
          </p>
        )}
      </ConfirmDialog>
      {confirm.dialog}
    </div>
  );
});

/** Admin-only settings: visibility, linked repository, delete. */
const Settings = observer(function Settings({
  detail,
  onPatch,
  onDelete,
  onVisibility,
}: {
  detail: PackageDetail;
  onPatch: (body: PackagePatch, success: string) => Promise<void>;
  onDelete: () => void;
  onVisibility: (v: 'public' | 'private') => void;
}) {
  const pkg = detail.package;
  const repos = reposForOwner(pkg.owner.id).map((r) => r.name);
  const [repo, setRepo] = useState('');
  const [busy, setBusy] = useState(false);
  const run = async (body: PackagePatch, success: string) => {
    setBusy(true);
    try {
      await onPatch(body, success);
      setRepo('');
    } catch (err) {
      toast({ kind: 'error', title: 'Could not update the package', description: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };
  const nextVisibility = pkg.visibility === 'public' ? 'private' : 'public';
  return (
    <>
      <Panel title="Package settings">
        <div className={styles.settings}>
          <div className={styles.dangerRow}>
            <div>
              <strong>Visibility</strong>
              <span className={`${styles.muted} ${styles.small}`}>This package is {pkg.visibility}.</span>
            </div>
            <Button size="sm" onClick={() => onVisibility(nextVisibility)}>
              Make {nextVisibility}
            </Button>
          </div>
          <div className={styles.dangerRow}>
            <div>
              <strong>Repository</strong>
              <span className={`${styles.muted} ${styles.small}`}>
                {pkg.repository ? (
                  <>
                    Linked to <Link to={`/${pkg.repository.full_name}`}>{pkg.repository.full_name}</Link>; it shows up in the repository’s sidebar.
                  </>
                ) : (
                  'Link a repository to show this package on its page.'
                )}
              </span>
            </div>
            {pkg.repository && (
              <Button size="sm" loading={busy} onClick={() => void run({ repository: null }, 'Repository unlinked')}>
                Unlink
              </Button>
            )}
          </div>
          <form
            className={styles.settingRow}
            onSubmit={(e) => {
              e.preventDefault();
              if (repo.trim()) void run({ repository: repo.trim() }, `Linked to ${pkg.owner.login}/${repo.trim()}`);
            }}
          >
            {repos.length > 0 ? (
              <Select aria-label="Repository to link" value={repo} onChange={(e) => setRepo(e.target.value)}>
                <option value="">Select a repository…</option>
                {repos.map((r) => (
                  <option key={r} value={r} disabled={r === pkg.repository?.name}>
                    {pkg.owner.login}/{r}
                  </option>
                ))}
              </Select>
            ) : (
              <Input aria-label="Repository to link" placeholder="repository name" value={repo} onChange={(e) => setRepo(e.target.value)} />
            )}
            <Button type="submit" size="md" disabled={!repo.trim()} loading={busy}>
              {pkg.repository ? 'Change repository' : 'Link repository'}
            </Button>
          </form>
        </div>
      </Panel>
      <Panel title="Danger zone" danger>
        <div className={styles.dangerRow}>
          <div>
            <strong>Delete this package</strong>
            <span className={`${styles.muted} ${styles.small}`}>Deletes the package and all of its versions.</span>
          </div>
          <Button variant="danger" size="sm" onClick={onDelete}>
            Delete this package
          </Button>
        </div>
      </Panel>
    </>
  );
});
