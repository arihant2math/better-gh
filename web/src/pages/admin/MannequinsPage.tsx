import { refresh, useResource } from '../../api/cache';
import { MANNEQUIN_KEYS, listAdminMannequins } from '../../api/mannequins';
import { ErrorState, PageHeader, Panel } from '../../components/admin/kit';
import { Link } from '../../router';
import { Skeleton } from '../../ui/EmptyState';
import { MannequinList } from '../imports/MannequinList';

/** `/site-admin/mannequins`: every mannequin on the instance, with reclaim. */
export default function MannequinsPage() {
  const list = useResource(MANNEQUIN_KEYS.admin, listAdminMannequins);
  const reload = () => void refresh(MANNEQUIN_KEYS.admin, listAdminMannequins).catch(() => undefined);
  return (
    <>
      <PageHeader
        title="Mannequins"
        description={
          <>
            Placeholder accounts <Link to="/site-admin/imports">imports</Link> created for source users without an account. They can't sign in. Invite the real person to reclaim one;
            its contributions move when they accept.
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
