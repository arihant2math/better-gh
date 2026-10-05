/**
 * GitHub Apps installed on one account (user settings
 * `/settings/installations`, org settings
 * `/organizations/:org/settings/installations`): list and configure
 * (repository access, permission upgrades, suspend, uninstall).
 */
import { useState } from 'react';
import {
  acceptPermissions,
  getInstallation,
  listInstallations,
  setSuspended,
  uninstall,
  updateInstallation,
  type InstallationDetail,
} from '../../api/apps';
import { invalidate, useResource } from '../../api/cache';
import { Banner, ButtonRow, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, Section, errorMessage } from '../../components/settings/kit';
import { Link, navigate } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, AppsIcon, ArrowLeftIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { AppIcon } from './AppsManager';
import styles from './apps.module.css';
import { ACCESS_LABEL, permissionChanges, permissionEntries, permissionLabel } from './logic';
import { RepoAccessPicker, type RepoSelection } from './RepoAccessPicker';

const listKey = (account: string) => `apps:installations:${account}`;
const detailKey = (id: number) => `apps:installation:${id}`;

export function InstallationsManager({ account, base, sub }: { account: string; base: string; sub: string[] }) {
  const id = Number(sub[0]);
  if (sub[0] && Number.isInteger(id)) return <InstallationPage key={id} id={id} account={account} base={base} />;
  return <InstallationList account={account} base={base} />;
}

function InstallationList({ account, base }: { account: string; base: string }) {
  const res = useResource(listKey(account), () => listInstallations(account));
  const items = res.data;
  return (
    <>
      <PageHeader title="Installed GitHub Apps" description="GitHub Apps installed on this account act on the repositories you granted them, with the permissions you accepted." />
      {items ? (
        items.length === 0 ? (
          <EmptyState icon={AppsIcon} title="No installed GitHub Apps">
            Install an app from its page (<code>/apps/&lt;slug&gt;</code>) to give it access to repositories.
          </EmptyState>
        ) : (
          <ItemList aria-label="Installed GitHub Apps">
            {items.map((i) => (
              <ItemRow
                key={i.id}
                leading={<AppIcon name={i.app_slug} size={32} />}
                title={
                  <Link to={`${base}/${i.id}`} className={styles.titleLink}>
                    {i.app_slug}
                  </Link>
                }
                meta={
                  <span className={styles.meta}>
                    <span>{i.repository_selection === 'all' ? 'All repositories' : 'Selected repositories'}</span>
                    <span>Installed {new Date(i.created_at).toLocaleDateString()}</span>
                    {i.suspended_at && <Pill tone="warning">Suspended</Pill>}
                  </span>
                }
                actions={
                  <Button size="sm" onClick={() => navigate(`${base}/${i.id}`)}>
                    Configure
                  </Button>
                }
              />
            ))}
          </ItemList>
        )
      ) : res.error ? (
        <ItemList empty="Could not load installations." />
      ) : (
        <FormStack>
          <Skeleton width="50%" />
        </FormStack>
      )}
    </>
  );
}

function InstallationPage({ id, account, base }: { id: number; account: string; base: string }) {
  const res = useResource(detailKey(id), () => getInstallation(id));
  const [local, setLocal] = useState<InstallationDetail | undefined>(undefined);
  const d = local ?? res.data;
  const [selection, setSelection] = useState<RepoSelection | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmUninstall, setConfirmUninstall] = useState(false);
  const [confirmSuspend, setConfirmSuspend] = useState(false);

  if (!d) {
    return (
      <>
        <Link to={base} className={styles.back}>
          <ArrowLeftIcon size={16} /> Installed GitHub Apps
        </Link>
        {res.error ? <EmptyState title="Installation not found" /> : <Skeleton width="40%" height={24} />}
      </>
    );
  }
  const inst = d.installation;
  const current: RepoSelection = selection ?? {
    mode: inst.repository_selection,
    repos: d.repositories.map((r) => ({ id: r.id, full_name: r.full_name, private: r.private })),
  };
  const dirty =
    !!selection &&
    (selection.mode !== inst.repository_selection ||
      (selection.mode === 'selected' && selection.repos.map((r) => r.id).sort().join() !== d.repositories.map((r) => r.id).sort().join()));
  const reload = (next: InstallationDetail) => {
    setLocal(next);
    setSelection(null);
    invalidate(detailKey(id));
    invalidate(listKey(account));
  };
  const save = async () => {
    setBusy(true);
    try {
      const next = await updateInstallation(id, { repository_selection: current.mode, repository_ids: current.repos.map((r) => r.id) });
      reload(next);
      toast({ kind: 'success', title: 'Repository access updated' });
      if (next.setup_redirect) window.location.assign(next.setup_redirect);
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  const changes = permissionChanges(inst.permissions, d.requested_permissions);
  return (
    <>
      <Link to={base} className={styles.back}>
        <ArrowLeftIcon size={16} /> Installed GitHub Apps
      </Link>
      <PageHeader
        title={
          <span className={styles.header}>
            <AppIcon name={d.app.name} />
            {d.app.name}
          </span>
        }
        description={
          <>
            Installed {new Date(inst.created_at).toLocaleDateString()} on <strong>{inst.account.login}</strong>. Developed by{' '}
            <Link to={`/${d.app.owner.login}`}>{d.app.owner.login}</Link>.
          </>
        }
      />
      {inst.suspended_at && (
        <Banner tone="warning" icon={AlertIcon}>
          This installation is suspended{inst.suspended_by ? ` by ${inst.suspended_by.login}` : ''}: the app can’t access this account.
        </Banner>
      )}
      {d.permissions_outdated && (
        <Section title="Permission update requested" description={`${d.app.name} is requesting updated permissions.`}>
          <ul className={styles.changeList}>
            {changes.map((c) => (
              <li key={c.key}>
                <strong>{permissionLabel(c.key)}</strong>: {c.from ? ACCESS_LABEL[c.from] : 'No access'} → {c.to ? ACCESS_LABEL[c.to] : 'No access'}
              </li>
            ))}
            {changes.length === 0 && <li>New webhook events: {d.requested_events.join(', ') || 'none'}</li>}
          </ul>
          <ButtonRow>
            <Button
              variant="primary"
              onClick={() =>
                void acceptPermissions(id).then(
                  (next) => {
                    reload(next);
                    toast({ kind: 'success', title: 'New permissions accepted' });
                  },
                  (e) => toast({ kind: 'error', title: errorMessage(e) }),
                )
              }
            >
              Accept new permissions
            </Button>
          </ButtonRow>
        </Section>
      )}
      <Section title="Permissions">
        <ul className={styles.permSummary} aria-label="Granted permissions">
          <li>
            <strong>Read</strong> access to metadata
          </li>
          {permissionEntries(inst.permissions).map(([k, a]) => (
            <li key={k}>
              <strong>{ACCESS_LABEL[a]}</strong> access to {permissionLabel(k).toLowerCase()}
            </li>
          ))}
        </ul>
      </Section>
      <Section title="Repository access">
        <RepoAccessPicker account={inst.account} value={current} onChange={setSelection} />
        <ButtonRow>
          <Button variant="primary" loading={busy} disabled={!dirty || (current.mode === 'selected' && current.repos.length === 0)} onClick={() => void save()}>
            Save
          </Button>
          {dirty && <Button onClick={() => setSelection(null)}>Cancel</Button>}
        </ButtonRow>
      </Section>
      <Section danger title="Danger zone">
        <div className={styles.dangerRow}>
          <div>
            <strong>{inst.suspended_at ? 'Unsuspend' : 'Suspend'} your installation</strong>
            <p className={styles.hint}>
              {inst.suspended_at ? 'Give the app its access back.' : 'Block the app’s access to this account (its tokens are revoked) without uninstalling it.'}
            </p>
          </div>
          <Button
            variant={inst.suspended_at ? 'secondary' : 'danger'}
            onClick={() =>
              inst.suspended_at
                ? void setSuspended(id, false).then(() => getInstallation(id).then(reload))
                : setConfirmSuspend(true)
            }
          >
            {inst.suspended_at ? 'Unsuspend' : 'Suspend'}
          </Button>
        </div>
        <div className={styles.dangerRow}>
          <div>
            <strong>Uninstall “{d.app.name}”</strong>
            <p className={styles.hint}>Removes the app’s access to every repository of this account.</p>
          </div>
          <Button variant="danger" onClick={() => setConfirmUninstall(true)}>
            Uninstall
          </Button>
        </div>
      </Section>
      <ConfirmDialog
        open={confirmSuspend}
        onClose={() => setConfirmSuspend(false)}
        title={`Suspend ${d.app.name}?`}
        confirmLabel="Suspend"
        onConfirm={async () => {
          await setSuspended(id, true);
          reload(await getInstallation(id));
        }}
      >
        <p>The app loses access until the installation is unsuspended.</p>
      </ConfirmDialog>
      <ConfirmDialog
        open={confirmUninstall}
        onClose={() => setConfirmUninstall(false)}
        title={`Uninstall ${d.app.name}?`}
        confirmLabel="Uninstall"
        onConfirm={async () => {
          await uninstall(id);
          invalidate(listKey(account));
          toast({ kind: 'success', title: `Uninstalled ${d.app.name}` });
          navigate(base);
        }}
      >
        <p>This removes all of its access to {inst.account.login}.</p>
      </ConfirmDialog>
    </>
  );
}
