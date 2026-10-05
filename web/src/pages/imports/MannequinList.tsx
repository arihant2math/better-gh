import { useId, useState, type FormEvent } from 'react';
import { cancelReclaim, inviteReclaim, type Mannequin } from '../../api/mannequins';
import { StatusPill, attempt } from '../../components/admin/kit';
import { apiFieldErrors } from '../../components/settings/kit';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { PersonIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import s from './imports.module.css';

const LOGIN_RE = /^@?[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/;

/** One mannequin: its source identity, reclaim state and the invite form. */
function Row({ m, onChange }: { m: Mannequin; onChange: () => void }) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [login, setLogin] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const target = login.trim().replace(/^@/, '');

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!LOGIN_RE.test(login.trim())) {
      setError('Enter the username of the person behind this mannequin');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await inviteReclaim(m.id, target);
      setOpen(false);
      setLogin('');
      onChange();
    } catch (err) {
      const { message, fields } = apiFieldErrors(err);
      setError((fields as Record<string, string>).login ?? message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className={s.item} data-testid="mannequin">
      <div className={s.itemMain}>
        <div className={s.itemTitle}>
          <Avatar user={{ login: m.login, avatarUrl: m.avatar_url }} size={20} />
          <strong>{m.source_login ?? m.login}</strong>
          <span className={s.muted}>
            {m.source ? `on ${m.source}` : ''} · {m.login}
          </span>
          {m.reclaimed_by ? (
            <StatusPill status="ok">Reclaimed</StatusPill>
          ) : m.pending_reclaim ? (
            <StatusPill status="info">Invitation pending</StatusPill>
          ) : (
            <StatusPill status="neutral">Mannequin</StatusPill>
          )}
        </div>
        <div className={s.muted}>
          {m.reclaimed_by ? (
            <>
              Contributions moved to <Link to={`/${m.reclaimed_by.login}`}>@{m.reclaimed_by.login}</Link>
            </>
          ) : m.pending_reclaim ? (
            <>
              Waiting for {m.pending_reclaim.target ? <Link to={`/${m.pending_reclaim.target.login}`}>@{m.pending_reclaim.target.login}</Link> : 'the invitee'} to accept · invited{' '}
              <RelativeTime date={m.pending_reclaim.created_at} />
            </>
          ) : (
            <>
              Created by an import <RelativeTime date={m.created_at} />
            </>
          )}
        </div>
        {open && (
          <form className={s.reclaimForm} onSubmit={(e) => void submit(e)} aria-label={`Reclaim ${m.source_login ?? m.login}`}>
            <label htmlFor={`${id}-login`} className={s.muted}>
              Real account
            </label>
            <Input
              id={`${id}-login`}
              value={login}
              onChange={(e) => setLogin(e.target.value)}
              placeholder="username"
              autoComplete="off"
              invalid={!!error}
              aria-describedby={error ? `${id}-err` : undefined}
              autoFocus
            />
            <Button type="submit" variant="primary" size="sm" disabled={busy || !target}>
              {busy ? 'Inviting…' : 'Send invitation'}
            </Button>
            <Button type="button" size="sm" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            {error && (
              <span id={`${id}-err`} className={s.fieldError} role="alert">
                {error}
              </span>
            )}
          </form>
        )}
      </div>
      {!m.reclaimed_by && !open && (
        <div className={s.rowActions}>
          {m.pending_reclaim ? (
            <Button size="sm" onClick={() => void attempt('Withdraw invitation', () => cancelReclaim(m.pending_reclaim!.id).then(onChange), 'Invitation withdrawn')}>
              Withdraw
            </Button>
          ) : (
            <Button size="sm" onClick={() => setOpen(true)}>
              Reclaim…
            </Button>
          )}
        </div>
      )}
    </li>
  );
}

/**
 * Mannequins with their reclaim state; "Reclaim…" invites a real account,
 * which then accepts in its settings (nothing moves before that).
 */
export function MannequinList({ rows, onChange }: { rows: Mannequin[]; onChange: () => void }) {
  if (!rows.length) {
    return (
      <EmptyState icon={PersonIcon} title="No mannequins">
        Imports create a mannequin for every source user without an account here.
      </EmptyState>
    );
  }
  return (
    <ul className={s.list} aria-label="Mannequins">
      {rows.map((m) => (
        <Row key={m.id} m={m} onChange={onChange} />
      ))}
    </ul>
  );
}
