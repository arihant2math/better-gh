import { useState } from 'react';
import { refresh, useResource } from '../../../api/cache';
import { MANNEQUIN_KEYS, acceptReclaim, declineReclaim, listMyReclaims, movedTotal, type Reclaim } from '../../../api/mannequins';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, Section, errorMessage, useAction } from '../../../components/settings/kit';
import { Link } from '../../../router';
import { Avatar } from '../../../ui/Badge';
import { Button } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { RelativeTime } from '../../../ui/RelativeTime';

const TONE: Record<Reclaim['status'], 'accent' | 'success' | 'neutral' | 'warning'> = {
  pending: 'accent',
  accepted: 'success',
  declined: 'neutral',
  cancelled: 'warning',
};

function who(r: Reclaim): string {
  const m = r.mannequin;
  return m ? `${m.source_login ?? m.login}${m.source ? ` (${m.source})` : ''}` : 'a mannequin';
}

/**
 * `/settings/reclaims`: invitations to take over a mannequin's imported
 * contributions. Accepting moves them to this account (it can't be undone).
 */
export default function ReclaimSettings() {
  const res = useResource(MANNEQUIN_KEYS.mine, listMyReclaims);
  const reload = () => void refresh(MANNEQUIN_KEYS.mine, listMyReclaims).catch(() => undefined);
  const [confirm, setConfirm] = useState<Reclaim | null>(null);
  const decline = useAction((id: number) => declineReclaim(id).then(reload), { success: 'Invitation declined' });
  const rows = res.data ?? [];
  const pending = rows.filter((r) => r.status === 'pending');
  const answered = rows.filter((r) => r.status !== 'pending');

  return (
    <>
      <PageHeader
        title="Imported contributions"
        description="When a repository is imported, people without an account here are represented by mannequins. If one of them is you, accept the invitation to have its issues, pull requests, reviews and comments attributed to your account."
      />
      {res.error && <Banner tone="danger">{errorMessage(res.error)}</Banner>}
      {!res.data && !res.error ? (
        <Skeleton height={80} />
      ) : (
        <>
          <Section title="Pending invitations">
            <ItemList aria-label="Pending invitations" empty="No pending invitations.">
              {pending.map((r) => (
                <ItemRow
                  key={r.id}
                  leading={<Avatar user={{ login: r.mannequin?.login ?? '', avatarUrl: r.mannequin?.avatar_url ?? '' }} size={32} />}
                  title={
                    <>
                      Claim <strong>{who(r)}</strong>
                    </>
                  }
                  meta={
                    <>
                      {r.invited_by ? `@${r.invited_by.login}` : 'An administrator'}
                      {r.organization && (
                        <>
                          {' '}
                          of <Link to={`/${r.organization.login}`}>{r.organization.login}</Link>
                        </>
                      )}{' '}
                      invited you <RelativeTime date={r.created_at} />
                    </>
                  }
                  actions={
                    <>
                      <Button size="sm" disabled={decline.busy} onClick={() => void decline.run(r.id)}>
                        Decline
                      </Button>
                      <Button size="sm" variant="primary" onClick={() => setConfirm(r)}>
                        Accept…
                      </Button>
                    </>
                  }
                />
              ))}
            </ItemList>
          </Section>
          {answered.length > 0 && (
            <Section title="History">
              <ItemList aria-label="Answered invitations">
                {answered.map((r) => (
                  <ItemRow
                    key={r.id}
                    title={
                      <>
                        {who(r)} <Pill tone={TONE[r.status]}>{r.status}</Pill>
                      </>
                    }
                    meta={
                      <>
                        {r.status === 'accepted' && `${movedTotal(r)} contributions moved · `}
                        <RelativeTime date={r.completed_at ?? r.updated_at} />
                      </>
                    }
                  />
                ))}
              </ItemList>
            </Section>
          )}
        </>
      )}
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title={confirm ? `Claim ${who(confirm)}?` : ''}
        confirmLabel="Accept and move contributions"
        danger={false}
        onConfirm={async () => {
          if (!confirm) return;
          await acceptReclaim(confirm.id);
          setConfirm(null);
          reload();
        }}
      >
        Everything this mannequin authored (issues, pull requests, reviews, comments, reactions) will show your account as its author. Only accept if these contributions are
        yours. This can't be undone.
      </ConfirmDialog>
    </>
  );
}
