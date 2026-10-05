import { useLocation, useParams } from '../../router';
import { Skeleton } from '../../ui/EmptyState';
import { OwnerRequired, useOrgAccess } from '../orgsettings/common';
import { AppsManager } from './AppsManager';
import { InstallationsManager } from './InstallationsManager';

/**
 * `/organizations/:org/settings/apps[/new|/:slug]` (GitHub Apps owned by
 * the org) and `/organizations/:org/settings/installations[/:id]`.
 */
export default function OrgAppsPage() {
  const { org = '' } = useParams<{ org: string }>();
  const { pathname } = useLocation();
  const access = useOrgAccess(org);
  const parts = pathname.split('/').filter(Boolean);
  // ['organizations', org, 'settings', 'apps' | 'installations', ...sub]
  const section = parts[3];
  const sub = parts.slice(4).map(decodeURIComponent);
  const base = `/organizations/${encodeURIComponent(org)}/settings/${section}`;
  if (access.loading) return <Skeleton width="40%" height={24} />;
  if (!access.isOwner) return <OwnerRequired org={org} what={section === 'apps' ? 'manage GitHub Apps' : 'manage installed GitHub Apps'} />;
  return section === 'apps' ? <AppsManager owner={org} base={base} sub={sub} /> : <InstallationsManager account={org} base={base} sub={sub} />;
}
