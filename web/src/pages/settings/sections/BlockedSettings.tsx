import { PageHeader } from '../../../components/settings/kit';
import { EmptyState } from '../../../ui/EmptyState';

/** TODO(account-web): Blocked users */
export default function Stub() {
  return (
    <>
      <PageHeader title="Blocked users" />
      <EmptyState title="Coming soon" />
    </>
  );
}
