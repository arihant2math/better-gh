import { useState } from 'react';
import { deleteAuthorization, listAuthorizations, type Authorization } from '@/api/developerSettings';
import { describeScope } from '@/api/scopes';
import { ConfirmDialog, ItemList, ItemRow, PageHeader, Section } from '@/components/settings/kit';
import { Button } from '@/ui/Button';
import { toast } from '@/ui/Toast';
import { ListSkeleton } from '../developer/common';
import styles from '../developer/developer.module.css';
import { formatDate } from '../developer/logic';
import { useList } from '@/api/useList';
import { formatRelative } from '@/ui/RelativeTime';

/** `/settings/applications`: OAuth apps the user has authorized. */
export default function ApplicationSettings() {
  const list = useList<Authorization>('dev:authorizations', listAuthorizations);
  const [confirm, setConfirm] = useState<Authorization | null>(null);
  const [confirmAll, setConfirmAll] = useState(false);
  const items = list.items;
  return (
    <>
      <PageHeader title="Applications" />
      <Section
        title={`Authorized OAuth Apps${items ? ` (${items.length})` : ''}`}
        description="You have granted these applications access to your account. Revoking one deletes its tokens; the app must ask again."
        actions={
          items && items.length > 1 ? (
            <Button size="sm" variant="danger" onClick={() => setConfirmAll(true)}>
              Revoke all
            </Button>
          ) : null
        }
      >
        {items ? (
          <ItemList aria-label="Authorized OAuth Apps" empty="No authorized applications. Apps you sign in to with your account appear here.">
            {items.map((g) => (
              <ItemRow
                key={g.id}
                leading={
                  <span className={styles.appAvatar} aria-hidden>
                    {g.app.name.charAt(0).toUpperCase()}
                  </span>
                }
                title={
                  g.app.url ? (
                    <a href={g.app.url} target="_blank" rel="noreferrer noopener" className={styles.titleLink}>
                      {g.app.name}
                    </a>
                  ) : (
                    g.app.name
                  )
                }
                actions={
                  <Button size="sm" variant="danger" onClick={() => setConfirm(g)} aria-label={`Revoke ${g.app.name}`}>
                    Revoke
                  </Button>
                }
              >
                <div className={styles.metaLines}>
                  <span className={styles.metaInline}>
                    <span>Granted {formatDate(g.created_at)}</span>
                    <span title={g.updated_at}>Last authorized {formatRelative(g.updated_at)}</span>
                    <span className={styles.mono}>{g.app.client_id}</span>
                  </span>
                </div>
                <ul className={styles.permList} aria-label="Permissions">
                  {g.scopes.length ? (
                    g.scopes.map((s) => (
                      <li key={s} className={styles.perm} title={s}>
                        {describeScope(s)}
                      </li>
                    ))
                  ) : (
                    <li className={styles.scopeDesc}>Read-only access to public information</li>
                  )}
                </ul>
              </ItemRow>
            ))}
          </ItemList>
        ) : list.error ? (
          <ItemList empty="Could not load your authorized applications." />
        ) : (
          <ListSkeleton />
        )}
      </Section>
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title={`Revoke ${confirm?.app.name ?? ''}?`}
        confirmLabel="I understand, revoke access"
        onConfirm={() => {
          const g = confirm!;
          void list.remove(g.id, () => deleteAuthorization(g.id), `Revoked access for ${g.app.name}`);
        }}
      >
        <p>
          <strong>{confirm?.app.name}</strong> will no longer be able to access your account, and all of its tokens are deleted. You can authorize it again
          later.
        </p>
      </ConfirmDialog>
      <ConfirmDialog
        open={confirmAll}
        onClose={() => setConfirmAll(false)}
        title="Revoke all OAuth applications"
        confirmLabel="I understand, revoke access for everything"
        confirmText="revoke all"
        onConfirm={async () => {
          const all = items ?? [];
          const ok = await Promise.all(all.map((g) => list.remove(g.id, () => deleteAuthorization(g.id))));
          const n = ok.filter(Boolean).length;
          toast({
            kind: n === all.length ? 'success' : 'error',
            title: `Revoked ${n} of ${all.length} applications`,
          });
        }}
      >
        <p>Every application listed here loses access to your account immediately.</p>
      </ConfirmDialog>
    </>
  );
}
