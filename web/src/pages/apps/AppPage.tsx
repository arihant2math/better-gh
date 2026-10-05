/**
 * `/apps/:slug`: a GitHub App's public page, and
 * `/apps/:slug/installations/new`: the install flow (pick the account,
 * then all or selected repositories, review permissions, install).
 */
import { useState } from 'react';
import { getInstallInfo, installApp, type InstallInfo, type SimpleUser } from '../../api/apps';
import { invalidate, useResource } from '../../api/cache';
import { Banner, ButtonRow, errorMessage, PageHeader, Section } from '../../components/settings/kit';
import { Link, navigate, useLocation, useParams } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, ArrowLeftIcon, CheckIcon, LinkExternalIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { AppIcon } from './AppsManager';
import styles from './apps.module.css';
import { ACCESS_LABEL, installationPath, permissionEntries, permissionLabel } from './logic';
import { RepoAccessPicker, type RepoSelection } from './RepoAccessPicker';

export default function AppPage() {
  const { slug = '' } = useParams<{ slug: string }>();
  const { pathname } = useLocation();
  const installing = pathname.endsWith('/installations/new');
  const res = useResource(`apps:install:${slug}`, () => getInstallInfo(slug));
  const info = res.data;
  if (!info) {
    return (
      <div className={styles.page}>
        {res.error ? (
          <EmptyState title="GitHub App not found">This app doesn’t exist, or it is private.</EmptyState>
        ) : (
          <>
            <Skeleton width="40%" height={28} />
            <Skeleton width="70%" />
          </>
        )}
      </div>
    );
  }
  return <div className={styles.page}>{installing ? <InstallFlow info={info} /> : <AppOverview info={info} />}</div>;
}

function Permissions({ info }: { info: InstallInfo }) {
  return (
    <ul className={styles.permSummary} aria-label="Requested permissions">
      <li>
        <CheckIcon size={14} /> <strong>Read</strong> access to metadata
      </li>
      {permissionEntries(info.app.permissions).map(([k, a]) => (
        <li key={k}>
          <CheckIcon size={14} /> <strong>{ACCESS_LABEL[a]}</strong> access to {permissionLabel(k).toLowerCase()}
        </li>
      ))}
    </ul>
  );
}

function AppOverview({ info }: { info: InstallInfo }) {
  const app = info.app;
  return (
    <>
      <PageHeader
        title={
          <span className={styles.header}>
            <AppIcon name={app.name} size={56} />
            {app.name}
          </span>
        }
        description={app.description ?? undefined}
        actions={
          <Button variant="primary" onClick={() => navigate(`/apps/${app.slug}/installations/new`)}>
            Install
          </Button>
        }
      />
      <div className={styles.overview}>
        <Section title="Permissions">
          <Permissions info={info} />
          {app.events.length > 0 && (
            <p className={styles.hint}>
              Receives webhooks for: {app.events.map((e) => <code key={e}>{e}</code>).reduce<React.ReactNode[]>((acc, x, i) => (i ? [...acc, ', ', x] : [x]), [])}
            </p>
          )}
        </Section>
        <aside className={styles.aside}>
          <h3>Developer</h3>
          <Link to={`/${app.owner.login}`} className={styles.ownerLink}>
            <Avatar user={{ login: app.owner.login, avatarUrl: app.owner.avatar_url }} size={20} /> {app.owner.login}
          </Link>
          <a href={info.homepage_url} target="_blank" rel="noreferrer noopener" className={styles.ownerLink}>
            <LinkExternalIcon size={14} /> Website
          </a>
          <p className={styles.hint}>{info.public ? 'Public app' : 'Private app: only its owner can install it.'}</p>
        </aside>
      </div>
    </>
  );
}

function InstallFlow({ info }: { info: InstallInfo }) {
  const app = info.app;
  const [account, setAccount] = useState<SimpleUser | null>(info.accounts.length === 1 ? info.accounts[0]!.account : null);
  const [selection, setSelection] = useState<RepoSelection>({ mode: 'all', repos: [] });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const existing = account && info.accounts.find((a) => a.account.id === account.id)?.installation_id;

  const install = async () => {
    if (!account) return;
    setBusy(true);
    setError(null);
    try {
      const d = await installApp(app.slug, {
        account: account.login,
        repository_selection: selection.mode,
        repository_ids: selection.repos.map((r) => r.id),
      });
      invalidate(`apps:install:${app.slug}`);
      invalidate('apps:installations:');
      toast({ kind: 'success', title: `Installed ${app.name} on ${account.login}` });
      if (d.setup_redirect) window.location.assign(d.setup_redirect);
      else navigate(installationPath(account, d.installation.id));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Link to={`/apps/${app.slug}`} className={styles.back}>
        <ArrowLeftIcon size={16} /> {app.name}
      </Link>
      <PageHeader
        title={
          <span className={styles.header}>
            <AppIcon name={app.name} />
            Install {app.name}
          </span>
        }
      />
      {!account ? (
        <Section title="Where do you want to install this app?">
          {info.accounts.length === 0 ? (
            <EmptyState title="No eligible accounts">This app is private and you don’t administer its owner.</EmptyState>
          ) : (
            <ul className={styles.accounts} aria-label="Accounts">
              {info.accounts.map((a) => (
                <li key={a.account.id}>
                  <Avatar user={{ login: a.account.login, avatarUrl: a.account.avatar_url }} size={32} square={a.account.type === 'Organization'} />
                  <span className={styles.accountName}>{a.account.login}</span>
                  {a.installation_id ? (
                    <Button size="sm" onClick={() => navigate(installationPath(a.account, a.installation_id!))}>
                      Configure
                    </Button>
                  ) : (
                    <Button size="sm" variant="primary" onClick={() => setAccount(a.account)}>
                      Install
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Section>
      ) : existing ? (
        <Banner tone="info" icon={AlertIcon}>
          {app.name} is already installed on {account.login}. <Link to={installationPath(account, existing)}>Configure it</Link>.
        </Banner>
      ) : (
        <>
          <Section title={`Install on ${account.login}`} description={info.accounts.length > 1 ? undefined : `${account.login} is the only account you can install this app on.`}>
            <RepoAccessPicker account={account} value={selection} onChange={setSelection} />
          </Section>
          <Section title="with these permissions:">
            <Permissions info={info} />
          </Section>
          {error && (
            <Banner tone="danger" icon={AlertIcon}>
              {error}
            </Banner>
          )}
          <ButtonRow>
            <Button variant="primary" loading={busy} disabled={selection.mode === 'selected' && selection.repos.length === 0} onClick={() => void install()}>
              Install
            </Button>
            {info.accounts.length > 1 && <Button onClick={() => setAccount(null)}>Choose another account</Button>}
            <Button variant="ghost" onClick={() => navigate(`/apps/${app.slug}`)}>
              Cancel
            </Button>
          </ButtonRow>
        </>
      )}
    </>
  );
}
