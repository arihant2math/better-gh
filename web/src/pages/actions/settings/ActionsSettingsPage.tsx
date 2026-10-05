import { observer } from 'mobx-react-lite';
import type { SettingsScope } from '../../../api/actions';
import { useLocation, useParams } from '../../../router';
import { store } from '../../../sync';
import { repoByName } from '../../../sync/selectors';
import { EmptyState } from '../../../ui/EmptyState';
import { CodeIcon, LockIcon, OrganizationIcon, RocketIcon, ServerIcon } from '../../../ui/icons';
import { ConfigList, OrgItemsForRepo } from './ConfigItems';
import { EnvironmentGroups, Environments } from './Environments';
import { ActionsNav, orgNavItems, type NavItem } from './nav';
import { Runners } from './Runners';
import styles from './Settings.module.css';

type SectionId = 'secrets' | 'variables' | 'runners' | 'environments';

const TITLES: Record<SectionId, string> = {
  secrets: 'Actions secrets',
  variables: 'Actions variables',
  runners: 'Runners',
  environments: 'Environments',
};

const DESCRIPTIONS: Record<SectionId, string> = {
  secrets:
    'Secrets are encrypted in your browser with the scope’s public key before they leave it. Their values are never shown again, and workflows read them as secrets.NAME.',
  variables: 'Variables are non-sensitive configuration values, stored as plain text. Workflows read them as vars.NAME.',
  runners: 'Self-hosted runners execute workflow jobs on machines you control.',
  environments: 'Environments group secrets and variables for a deployment target.',
};

function sectionOf(pathname: string): SectionId {
  if (pathname.includes('/secrets/')) return 'secrets';
  if (pathname.includes('/variables/')) return 'variables';
  if (pathname.endsWith('/environments')) return 'environments';
  return 'runners';
}

export default function ActionsSettingsPage() {
  const params = useParams<{ owner?: string; repo?: string; org?: string }>();
  const { pathname } = useLocation();
  const section = sectionOf(pathname);
  if (params.org) return <OrgSettings org={params.org} section={section} />;
  return <RepoSettings owner={params.owner ?? ''} repo={params.repo ?? ''} section={section} />;
}

const RepoSettings = observer(function RepoSettings({ owner, repo, section }: { owner: string; repo: string; section: SectionId }) {
  const r = repoByName(owner, repo);
  const canAdmin = !!r && store().get('viewerRepo', r.id)?.permission === 'admin';
  const base = `/${r?.owner ?? owner}/${r?.name ?? repo}`;
  const scope: SettingsScope = { kind: 'repo', owner: r?.owner ?? owner, repo: r?.name ?? repo };
  const envLink = `${base}/settings/environments`;
  const items: NavItem[] = [
    { id: 'secrets', label: 'Secrets', icon: LockIcon, to: `${base}/settings/secrets/actions` },
    { id: 'variables', label: 'Variables', icon: CodeIcon, to: `${base}/settings/variables/actions` },
    { id: 'runners', label: 'Runners', icon: ServerIcon, to: `${base}/settings/actions/runners` },
    { id: 'environments', label: 'Environments', icon: RocketIcon, to: envLink },
  ];

  let content;
  if (!canAdmin) {
    content = (
      <EmptyState icon={LockIcon} title="You need admin access to manage Actions settings">
        Secrets, variables, runners and environments of {owner}/{repo} can only be viewed and changed by repository administrators.
      </EmptyState>
    );
  } else if (section === 'secrets' || section === 'variables') {
    const noun = section === 'secrets' ? 'secret' : 'variable';
    content = (
      <>
        <ConfigList
          scope={scope}
          kind={section}
          title={`Repository ${section}`}
          description={`Available to every workflow in this repository. Environment ${noun}s with the same name take precedence.`}
        />
        <EnvironmentGroups owner={scope.owner} repo={scope.repo} kind={section} envLink={envLink} />
        <OrgItemsForRepo owner={scope.owner} repo={scope.repo} kind={section} />
      </>
    );
  } else if (section === 'runners') {
    content = <Runners scope={scope} />;
  } else {
    content = <Environments owner={scope.owner} repo={scope.repo} />;
  }

  return (
    <div className={styles.page}>
      <ActionsNav items={items} current={section} back={{ to: `${base}/actions`, label: 'Back to Actions' }} />
      <div className={styles.content}>
        <header className={styles.header}>
          <h1 className={styles.title}>{TITLES[section]}</h1>
          <p className={styles.desc}>{DESCRIPTIONS[section]}</p>
        </header>
        <div key={section} className={styles.sections}>
          {content}
        </div>
      </div>
    </div>
  );
});

function OrgSettings({ org, section: raw }: { org: string; section: SectionId }) {
  const section = raw === 'environments' ? 'runners' : raw;
  const scope: SettingsScope = { kind: 'org', org };
  const items = orgNavItems(org);
  return (
    <div className={styles.page}>
      <ActionsNav items={items} current={section} back={{ to: `/${org}`, label: org }} />
      <div className={styles.content}>
        <header className={styles.header}>
          <div className={styles.orgCrumb}>
            <OrganizationIcon size={16} />
            {org} · Organization settings
          </div>
          <h1 className={styles.title}>{TITLES[section]}</h1>
          <p className={styles.desc}>{DESCRIPTIONS[section]}</p>
        </header>
        <div key={section} className={styles.sections}>
        {section === 'runners' ? (
          <Runners scope={scope} />
        ) : (
          <ConfigList
            scope={scope}
            kind={section}
            title={`Organization ${section}`}
            description={`Choose which repositories can use each ${section === 'secrets' ? 'secret' : 'variable'}. Repository and environment values with the same name take precedence.`}
          />
        )}
        </div>
      </div>
    </div>
  );
}
