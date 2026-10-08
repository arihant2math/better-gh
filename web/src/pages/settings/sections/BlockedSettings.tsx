import { useEffect, useId, useState, type FormEvent } from 'react';
import { ApiError } from '@/api/client';
import { KEYS, blockUser, getUser, listBlocks, unblockUser, useEditableResource, type BlockedUser, type PublicUser } from '@/api/userSettings';
import { session } from '@/app/session';
import { Banner, ItemList, ItemRow, PageHeader, Section, errorMessage, useDebounced } from '@/components/settings/kit';
import { Link } from '@/router';
import { Avatar } from '@/ui/Badge';
import { Button } from '@/ui/Button';
import { Skeleton } from '@/ui/EmptyState';
import { BlockedIcon, CheckIcon, PersonIcon } from '@/ui/icons';
import { Field, Input } from '@/ui/Input';
import { Spinner } from '@/ui/Spinner';
import { toast } from '@/ui/Toast';
import styles from './userSettings.module.css';

const LOGIN_RE = /^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/;

type Lookup = { state: 'idle' } | { state: 'loading' } | { state: 'found'; user: PublicUser } | { state: 'missing' } | { state: 'error'; message: string };

export default function BlockedSettings() {
  const res = useEditableResource(KEYS.blocks, listBlocks);
  const [q, setQ] = useState('');
  const [lookup, setLookup] = useState<Lookup>({ state: 'idle' });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pending, setPending] = useState<Set<string>>(new Set());
  const inputId = useId();
  const login = q.trim().replace(/^@/, '');
  const debounced = useDebounced(login, 250);

  // Validate the username against the server as the user types.
  useEffect(() => {
    if (!debounced || !LOGIN_RE.test(debounced)) {
      setLookup({ state: 'idle' });
      return;
    }
    const ctl = new AbortController();
    setLookup({ state: 'loading' });
    getUser(debounced, ctl.signal).then(
      (user) => setLookup({ state: 'found', user }),
      (e: unknown) => {
        if (ctl.signal.aborted) return;
        if (e instanceof ApiError && e.status === 404) setLookup({ state: 'missing' });
        else setLookup({ state: 'error', message: errorMessage(e) });
      },
    );
    return () => ctl.abort();
  }, [debounced]);

  const blocked = res.data ?? [];
  const already = blocked.some((b) => b.login.toLowerCase() === login.toLowerCase());

  const validate = (): string | null => {
    if (!login) return 'Enter a username';
    if (!LOGIN_RE.test(login)) return 'Usernames may only contain letters, numbers and single hyphens';
    if (login.toLowerCase() === session.user?.login.toLowerCase()) return "You can't block yourself";
    if (already) return `@${login} is already blocked`;
    if (lookup.state === 'missing' && debounced === login) return `User @${login} not found`;
    if (lookup.state === 'found' && lookup.user.type === 'Organization') return "Organizations can't be blocked";
    return null;
  };

  const submit = async (ev: FormEvent) => {
    ev.preventDefault();
    if (busy) return;
    const err = validate();
    setError(err);
    if (err) return;
    setBusy(true);
    try {
      const user = lookup.state === 'found' && lookup.user.login.toLowerCase() === login.toLowerCase() ? lookup.user : await getUser(login);
      if (user.type === 'Organization') throw new Error("Organizations can't be blocked");
      await blockUser(user.login);
      res.update((l) => [...l, { login: user.login, id: user.id, avatar_url: user.avatar_url, name: user.name ?? null, type: user.type }]);
      setQ('');
      setLookup({ state: 'idle' });
      toast({ kind: 'success', title: `Blocked @${user.login}` });
    } catch (e) {
      setError(e instanceof ApiError && e.status === 404 ? `User @${login} not found` : errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const unblock = async (u: BlockedUser) => {
    setPending((p) => new Set(p).add(u.login));
    res.update((l) => l.filter((x) => x.id !== u.id));
    try {
      await unblockUser(u.login);
      toast({ kind: 'success', title: `Unblocked @${u.login}` });
    } catch (e) {
      res.update((l) => [...l, u]);
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setPending((p) => {
        const n = new Set(p);
        n.delete(u.login);
        return n;
      });
    }
  };

  const preview =
    lookup.state === 'loading' ? (
      <span className={styles.lookup}>
        <Spinner size={14} /> Looking up @{login}…
      </span>
    ) : lookup.state === 'found' && debounced === login ? (
      <span className={styles.lookup} data-testid="block-preview">
        <Avatar user={{ login: lookup.user.login, name: lookup.user.name, avatarUrl: lookup.user.avatar_url }} size={20} />
        <strong>{lookup.user.login}</strong>
        {lookup.user.name && <span className={styles.muted}>{lookup.user.name}</span>}
        {lookup.user.type !== 'Organization' && !already && <CheckIcon size={14} className={styles.okIcon} />}
      </span>
    ) : lookup.state === 'missing' && debounced === login ? (
      <span className={styles.lookupMissing}>No user named @{login}</span>
    ) : null;

  return (
    <>
      <PageHeader title="Blocked users" description="Blocked users can't follow you, comment on or open issues and pull requests in your repositories, or invite you to organizations." />
      <Section title="Block a user">
        <form onSubmit={(e) => void submit(e)} noValidate aria-label="Block a user">
          <Field label="Username" htmlFor={inputId} error={error} hint={preview ?? 'Search by username.'}>
            <div className={styles.inlineRow}>
              <Input
                id={inputId}
                className={styles.grow}
                leadingIcon={PersonIcon}
                placeholder="Search by username"
                value={q}
                invalid={!!error}
                autoComplete="off"
                spellCheck={false}
                onChange={(e) => {
                  setQ(e.target.value);
                  setError(null);
                }}
              />
              <Button type="submit" variant="danger" leadingIcon={BlockedIcon} loading={busy}>
                Block user
              </Button>
            </div>
          </Field>
        </form>
      </Section>
      <Section title={`Blocked users${res.data ? ` (${blocked.length})` : ''}`}>
        {res.error && !res.data ? (
          <Banner tone="danger">{errorMessage(res.error)}</Banner>
        ) : !res.data ? (
          <Skeleton height={60} />
        ) : (
          <ItemList aria-label="Blocked users" empty="You have not blocked any users.">
            {blocked.map((u) => (
              <ItemRow
                key={u.id}
                leading={<Avatar user={{ login: u.login, name: u.name ?? null, avatarUrl: u.avatar_url }} size={32} />}
                title={
                  <Link to={`/${u.login}`} data-testid="blocked-login">
                    {u.login}
                  </Link>
                }
                meta={u.name ?? undefined}
                actions={
                  <Button size="sm" loading={pending.has(u.login)} onClick={() => void unblock(u)}>
                    Unblock
                  </Button>
                }
              />
            ))}
          </ItemList>
        )}
      </Section>
    </>
  );
}
