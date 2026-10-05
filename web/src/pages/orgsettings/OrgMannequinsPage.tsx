import { refresh, useResource } from '../../api/cache';
import { MANNEQUIN_KEYS, listOrgMannequins } from '../../api/mannequins';
import { ErrorState, PageHeader, Panel } from '../../components/admin/kit';
import { Link, useParams } from '../../router';
import { Skeleton } from '../../ui/EmptyState';
import { MannequinList } from '../imports/MannequinList';
import { OwnerRequired, useOrgAccess } from './common';
import { orgSettingsPath } from './OrgSettingsLayout';

/** `/organizations/:org/settings/mannequins`: reclaim the mannequins this organization's imports created. */
export default function OrgMannequinsPage() {
  const { org = '' } = useParams<{ org: string }>();
  const access = useOrgAccess(org);
  const key = MANNEQUIN_KEYS.org(org);
  const list = useResource(access.isOwner ? key : null, () => listOrgMannequins(org));
  if (!access.loading && !access.isOwner) return <OwnerRequired org={org} what="reclaim mannequins" />;
  const reload = () => void refresh(key, () => listOrgMannequins(org)).catch(() => undefined);
  return (
    <>
      <PageHeader
        title="Mannequins"
        description={
          <>
            Source users of your <Link to={orgSettingsPath(org, 'import')}>imports</Link> without an account here. Invite the real person to take over a mannequin: once they accept, its
            issues, pull requests, reviews, comments and reactions are attributed to them.
          </>
        }
      />
      {list.error ? (
        <ErrorState error={list.error} onRetry={reload} />
      ) : !list.data ? (
        <Skeleton height={120} />
      ) : (
        <Panel padded={false} title={`${list.data.length} ${list.data.length === 1 ? 'mannequin' : 'mannequins'}`}>
          <MannequinList rows={list.data} onChange={reload} />
        </Panel>
      )}
    </>
  );
}
