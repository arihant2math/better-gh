import { observer } from 'mobx-react-lite';
import { useId, useState } from 'react';
import { invalidate } from '../../../api/cache';
import { lifecycleKeys, listDeletedRepos } from '../../../api/lifecycle';
import { useEditableResource } from '../../../api/userSettings';
import { session } from '../../../app/session';
import { Banner, PageHeader, Section, errorMessage } from '../../../components/settings/kit';
import { hasSync, store } from '../../../sync';
import { Button } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { InfoIcon } from '../../../ui/icons';
import { Select } from '../../../ui/Input';
import { DeletedReposList } from './DeletedReposList';
import styles from './userSettings.module.css';

/** Accounts whose deleted repositories the viewer may restore: themselves and organizations they own. */
const ownedAccounts = (): string[] => {
  const me = session.user;
  if (!me) return [];
  const orgs = hasSync()
    ? store()
        .all('membership')
        .filter((m) => m.userId === me.id && m.role === 'admin')
        .map((m) => store().get('org', m.orgId)?.login)
        .filter((l): l is string => !!l)
        .sort((a, b) => a.localeCompare(b))
    : [];
  return [me.login, ...orgs];
};

/** `/settings/repositories/deleted`: repositories deleted within the last 90 days, with Restore. */
export default observer(function DeletedReposSettings() {
  const [owner, setOwner] = useState('');
  const id = useId();
  const accounts = ownedAccounts();
  const key = lifecycleKeys.deleted(owner || undefined);
  const list = useEditableResource(key, () => listDeletedRepos(owner || undefined));
  return (
    <>
      <PageHeader
        title="Deleted repositories"
        description="Repositories deleted in the last 90 days from your account or from organizations you own. After that they are purged for good."
        actions={
          accounts.length > 1 ? (
            <label className={styles.inlineRow} htmlFor={id}>
              <span className={styles.muted}>Owner</span>
              <Select id={id} value={owner} onChange={(e) => setOwner(e.target.value)} aria-label="Filter by owner">
                <option value="">All accounts</option>
                {accounts.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </Select>
            </label>
          ) : undefined
        }
      />
      <Section>
        {list.error ? (
          <Banner tone="danger">
            {errorMessage(list.error)}{' '}
            <Button size="sm" onClick={() => void list.refresh()}>
              Retry
            </Button>
          </Banner>
        ) : !list.data ? (
          <Skeleton height={64} />
        ) : (
          <DeletedReposList
            rows={list.data}
            empty={owner ? `${owner} has no deleted repositories that can be restored.` : 'No repositories were deleted in the last 90 days.'}
            onRestored={(row) => {
              list.update((rows) => rows.filter((r) => r.id !== row.id));
              invalidate('lifecycle:deleted:');
            }}
          />
        )}
        <p className={styles.small}>
          <InfoIcon size={14} /> Restoring brings the repository back under its original owner and name. A repository can’t be restored while its owner
          has another repository with the same name.
        </p>
      </Section>
    </>
  );
});
