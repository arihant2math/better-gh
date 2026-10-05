import { useEffect, useRef, useState } from 'react';
import { invalidate } from '../../../api/cache';
import { ApiError } from '../../../api/client';
import { acceptTransfer, declineTransfer, lifecycleKeys, listIncomingTransfers, type RepoTransfer } from '../../../api/lifecycle';
import { useEditableResource } from '../../../api/userSettings';
import { Banner, ItemList, ItemRow, Pill, PageHeader, Section, errorMessage } from '../../../components/settings/kit';
import { Link, navigate, useQuery } from '../../../router';
import { Avatar } from '../../../ui/Badge';
import { Button } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { CheckIcon, InfoIcon, XIcon } from '../../../ui/icons';
import { RelativeTime } from '../../../ui/RelativeTime';
import { toast } from '../../../ui/Toast';
import styles from './userSettings.module.css';

/** `/settings/repositories/transfers`: repositories other users want to transfer to you. `?id=` highlights one (the email links here). */
export default function TransfersSettings() {
  const list = useEditableResource(lifecycleKeys.incoming(), listIncomingTransfers);
  const highlight = Number(useQuery().get('id')) || null;
  const [expired, setExpired] = useState<Set<number>>(() => new Set());
  const drop = (id: number) => {
    list.update((rows) => rows.filter((r) => r.id !== id));
    invalidate(lifecycleKeys.incoming());
  };
  return (
    <>
      <PageHeader title="Repository transfers" description="Repositories other people asked to transfer to you. A request expires one day after it was made." />
      <Section>
        {list.error ? (
          <Banner tone="danger">{errorMessage(list.error)}</Banner>
        ) : !list.data ? (
          <Skeleton height={64} />
        ) : (
          <>
            {highlight !== null && !list.data.some((t) => t.id === highlight) && (
              <Banner tone="warning" icon={InfoIcon}>
                This transfer request no longer exists. It was accepted, declined, cancelled or it expired.
              </Banner>
            )}
            <ItemList aria-label="Incoming transfer requests" empty="You have no pending repository transfer requests.">
              {list.data.map((t) => (
                <TransferRow
                  key={t.id}
                  t={t}
                  highlighted={t.id === highlight}
                  expired={expired.has(t.id)}
                  onExpired={() => setExpired((s) => new Set(s).add(t.id))}
                  onDone={() => drop(t.id)}
                />
              ))}
            </ItemList>
          </>
        )}
      </Section>
    </>
  );
}

function TransferRow({ t, highlighted, expired, onExpired, onDone }: { t: RepoTransfer; highlighted: boolean; expired: boolean; onExpired: () => void; onDone: () => void }) {
  const [busy, setBusy] = useState<'accept' | 'decline' | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (highlighted) ref.current?.scrollIntoView({ block: 'center' });
  }, [highlighted]);
  const newName = t.new_name ?? t.repository.name;
  const target = `${t.to.login}/${newName}`;
  const run = async (kind: 'accept' | 'decline') => {
    setBusy(kind);
    try {
      if (kind === 'accept') {
        const repo = await acceptTransfer(t.id);
        toast({ kind: 'success', title: `${t.repository.full_name} is now ${repo.full_name ?? target}` });
        onDone();
        navigate(`/${repo.full_name ?? target}`);
      } else {
        await declineTransfer(t.id);
        toast({ title: `Declined the transfer of ${t.repository.full_name}` });
        onDone();
      }
    } catch (e) {
      if (e instanceof ApiError && e.status === 410) onExpired();
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };
  return (
    <ItemRow
      className={highlighted ? styles.highlightRow : undefined}
      leading={<Avatar user={{ login: t.from.login, avatarUrl: t.from.avatar_url }} size={32} />}
      title={
        <div ref={ref} className={styles.titleRow}>
          <span className={styles.strong}>{t.repository.full_name}</span>
          <span className={styles.muted}>→ {target}</span>
          {t.repository.private && <Pill>Private</Pill>}
          {expired && <Pill tone="danger">Expired</Pill>}
        </div>
      }
      meta={
        <>
          Requested by <Link to={`/${(t.requested_by ?? t.from).login}`}>{(t.requested_by ?? t.from).login}</Link> <RelativeTime date={t.created_at} /> ·{' '}
          {expired ? 'expired' : 'expires'} <RelativeTime date={t.expires_at} />
        </>
      }
      actions={
        <>
          <Button size="sm" leadingIcon={XIcon} disabled={!!busy || expired} loading={busy === 'decline'} onClick={() => void run('decline')}>
            Decline
          </Button>
          <Button size="sm" variant="primary" leadingIcon={CheckIcon} disabled={!!busy || expired} loading={busy === 'accept'} onClick={() => void run('accept')}>
            Accept
          </Button>
        </>
      }
    />
  );
}
