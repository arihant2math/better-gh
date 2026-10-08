import { useState } from 'react';
import { mutate, refresh, useResource } from '@/api/cache';
import { cancelTransfer, getPendingTransfer, lifecycleKeys, type RepoTransfer } from '@/api/lifecycle';
import { Banner, errorMessage } from '@/components/settings/kit';
import { Button } from '@/ui/Button';
import { ClockIcon } from '@/ui/icons';
import { RelativeTime } from '@/ui/RelativeTime';
import { toast } from '@/ui/Toast';
import styles from '../RepoSettings.module.css';

/** The repository's pending outgoing transfer (`GET /_bgh/repos/{o}/{r}/transfer`, `null` when none). */
export function usePendingTransfer(owner: string, name: string) {
  const key = lifecycleKeys.pending(owner, name);
  const res = useResource(key, () => getPendingTransfer(owner, name));
  return {
    transfer: res.data ?? null,
    reload: () => void refresh(key, () => getPendingTransfer(owner, name)).catch(() => undefined),
    clear: () => mutate<RepoTransfer | null>(key, () => null),
  };
}

/** Banner in the danger zone: who the repository waits for, until when, and Cancel. */
export function PendingTransferBanner({ owner, transfer, onCancelled }: { owner: string; transfer: RepoTransfer; onCancelled: () => void }) {
  const [busy, setBusy] = useState(false);
  const target = `${transfer.to.login}/${transfer.new_name ?? transfer.repository.name}`;
  const cancel = async () => {
    setBusy(true);
    try {
      await cancelTransfer(owner, transfer.repository.name);
      onCancelled();
      toast({ title: `Cancelled the transfer of ${transfer.repository.full_name}` });
    } catch (e) {
      toast({ kind: 'error', title: 'Couldn’t cancel the transfer', description: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Banner tone="warning" icon={ClockIcon}>
      <div data-testid="pending-transfer" className={styles.pendingRow}>
        <span className={styles.pendingText}>
          <strong>Transfer pending.</strong> {transfer.to.login} has been asked to accept this repository as <strong>{target}</strong>
          {transfer.requested_by && transfer.requested_by.login !== transfer.from.login ? ` (requested by ${transfer.requested_by.login})` : ''}. The request expires{' '}
          <RelativeTime date={transfer.expires_at} />.
        </span>
        <Button size="sm" loading={busy} onClick={() => void cancel()}>
          Cancel transfer
        </Button>
      </div>
    </Banner>
  );
}
