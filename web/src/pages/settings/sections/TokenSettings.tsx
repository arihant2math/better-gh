import { observer } from 'mobx-react-lite';
import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import { session } from '../../../app/session';
import { invalidate } from '../../../api/cache';
import { createToken, deleteToken, listTokens, type AccessToken } from '../../../api/developerSettings';
import { describeScope } from '../../../api/scopes';
import { apiFieldErrors, Banner, ButtonRow, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, Section } from '../../../components/settings/kit';
import { Link, navigate, useLocation, useQuery } from '../../../router';
import { Button, cx } from '../../../ui/Button';
import { AlertIcon, ArrowLeftIcon, KeyAsteriskIcon, PlusIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input, Select } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import { ListSkeleton, OneTimeSecret, lastUsedText, subPath } from '../developer/common';
import styles from '../developer/developer.module.css';
import { FineGrainedSection, NewFineGrainedToken } from '../developer/FineGrainedTokens';
import { dateInDays, expiresInDays, expiryStatus, formatDate, selectedScopes, type ExpiryChoice } from '../developer/logic';
import { ScopeTree } from '../developer/ScopeTree';
import { useList } from '../developer/useList';

const LIST_KEY = 'dev:tokens';

/** The token just created, kept in memory only until the list page unmounts. */
let justCreated: AccessToken | null = null;

/**
 * `/settings/tokens` (fine-grained + classic tokens), `/settings/tokens/new`
 * (classic) and `/settings/tokens/new?type=fine-grained`.
 */
export default function TokenSettings() {
  const { pathname } = useLocation();
  const query = useQuery();
  if (subPath(pathname)[0] !== 'new') return <TokenList />;
  return query.get('type') === 'fine-grained' ? <NewFineGrainedToken /> : <NewToken />;
}

function TokenList() {
  const list = useList<AccessToken>(LIST_KEY, listTokens, { prepend: true });
  const [created] = useState(() => justCreated);
  useEffect(() => {
    justCreated = null;
  }, []);
  const [confirm, setConfirm] = useState<AccessToken | null>(null);
  const [confirmAll, setConfirmAll] = useState(false);
  // The list may still be the cached one from before the token existed.
  const items = list.items && created && !list.items.some((t) => t.id === created.id) ? [created, ...list.items] : list.items;
  return (
    <>
      <PageHeader
        title="Personal access tokens"
        description={
          <>
            Tokens you have generated that can be used to access the API and Git over HTTPS (as the password). Use them with{' '}
            <code className={styles.code}>gh auth login --with-token</code>.
          </>
        }
      />
      {created && (
        <div style={{ marginBottom: 24 }}>
          <OneTimeSecret
            value={created.token!}
            label="New personal access token"
            warning="Make sure to copy your token now. You won’t be able to see it again!"
          />
        </div>
      )}
      <FineGrainedSection />
      <Section
        title="Tokens (classic)"
        description="Classic tokens are granted OAuth scopes and work for every account and organization you can access."
        actions={
          <>
            {items && items.length > 1 ? (
              <Button size="sm" variant="danger" onClick={() => setConfirmAll(true)}>
                Revoke all
              </Button>
            ) : null}
            <Button size="sm" leadingIcon={PlusIcon} onClick={() => navigate('/settings/tokens/new')}>
              Generate new token (classic)
            </Button>
          </>
        }
      >
        {items ? (
          <ItemList aria-label="Personal access tokens" empty="You have no personal access tokens yet.">
            {items.map((t) => (
              <TokenRow key={t.id} t={t} fresh={t.id === created?.id} onRevoke={() => setConfirm(t)} />
            ))}
          </ItemList>
        ) : list.error ? (
          <ItemList empty="Could not load your tokens." />
        ) : (
          <ListSkeleton />
        )}
        <p className={styles.help}>Personal access tokens function like passwords. Never share them, and revoke any you no longer use.</p>
      </Section>
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Revoke token"
        confirmLabel="I understand, revoke this token"
        onConfirm={() => {
          const t = confirm!;
          void list.remove(t.id, () => deleteToken(t.id), 'Token revoked');
        }}
      >
        <p>
          Any applications or scripts using <strong>{confirm?.name || 'this token'}</strong> will no longer be able to access the API. You cannot undo this
          action.
        </p>
      </ConfirmDialog>
      <ConfirmDialog
        open={confirmAll}
        onClose={() => setConfirmAll(false)}
        title="Revoke all personal access tokens"
        confirmLabel="I understand, revoke all tokens"
        confirmText="revoke all"
        onConfirm={async () => {
          const all = items ?? [];
          const results = await Promise.all(all.map((t) => list.remove(t.id, () => deleteToken(t.id))));
          const n = results.filter(Boolean).length;
          toast({
            kind: n === all.length ? 'success' : 'error',
            title: `Revoked ${n} of ${all.length} tokens`,
          });
        }}
      >
        <p>
          This revokes <strong>all {items?.length ?? 0} personal access tokens</strong>. Every script, CI job and tool using one of them stops working
          immediately.
        </p>
      </ConfirmDialog>
    </>
  );
}

function TokenRow({ t, fresh, onRevoke }: { t: AccessToken; fresh: boolean; onRevoke: () => void }) {
  const exp = expiryStatus(t.expires_at);
  return (
    <ItemRow
      icon={KeyAsteriskIcon}
      className={cx(fresh && styles.highlightRow)}
      title={
        <>
          <span>{t.name || <em>Untitled token</em>}</span>
          {exp.kind === 'expired' && <Pill tone="danger">Expired</Pill>}
          {exp.kind === 'soon' && <Pill tone="warning">Expires soon</Pill>}
          {exp.kind === 'never' && <Pill tone="warning">No expiration</Pill>}
          {fresh && <Pill tone="success">New</Pill>}
        </>
      }
      actions={
        <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={onRevoke} aria-label={`Revoke token ${t.name}`}>
          Revoke
        </Button>
      }
    >
      <div className={styles.scopeTags} aria-label="Scopes">
        {t.scopes.length ? (
          t.scopes.map((s) => (
            <span key={s} className={styles.scopeTag} title={describeScope(s)}>
              {s}
            </span>
          ))
        ) : (
          <span className={styles.scopeDesc}>No scopes — read-only access to public information</span>
        )}
      </div>
      <div className={styles.metaLines}>
        <span className={styles.metaInline}>
          <span>{lastUsedText(t.last_used_at)}</span>
          <span>
            {exp.kind === 'never'
              ? 'This token has no expiration date'
              : exp.kind === 'expired'
                ? `Expired on ${formatDate(exp.at)}`
                : `Expires on ${formatDate(exp.at)}`}
          </span>
          <span className={styles.mono}>…{t.token_last_eight}</span>
        </span>
      </div>
    </ItemRow>
  );
}

// ------------------------------------------------------------------ new token

const EXPIRY_OPTIONS: { value: ExpiryChoice; label: string }[] = [
  { value: '7', label: '7 days' },
  { value: '30', label: '30 days' },
  { value: '60', label: '60 days' },
  { value: '90', label: '90 days' },
  { value: 'custom', label: 'Custom…' },
  { value: 'none', label: 'No expiration' },
];

const NewToken = observer(function NewToken() {
  const id = useId();
  const [note, setNote] = useState('');
  const [expiry, setExpiry] = useState<ExpiryChoice>('30');
  const [custom, setCustom] = useState(() => dateInDays(30));
  const [scopes, setScopes] = useState<Set<string>>(() => new Set());
  const [errors, setErrors] = useState<{
    note?: string;
    expiry?: string;
    scopes?: string;
    form?: string;
  }>({});
  const [busy, setBusy] = useState(false);
  const noteRef = useRef<HTMLInputElement>(null);
  useEffect(() => noteRef.current?.focus(), []);
  const siteAdmin = !!(session.user as { siteAdmin?: boolean } | null)?.siteAdmin;

  const exp = expiresInDays(expiry, custom);
  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (busy) return;
    const errs: typeof errors = {};
    if (!note.trim()) errs.note = 'Note can’t be blank';
    if (exp.error) errs.expiry = exp.error;
    setErrors(errs);
    if (errs.note) return noteRef.current?.focus();
    if (errs.expiry) return;
    setBusy(true);
    try {
      const t = await createToken({
        name: note.trim(),
        scopes: selectedScopes(scopes),
        expires_in_days: exp.days,
      });
      justCreated = t;
      invalidate(LIST_KEY);
      navigate('/settings/tokens');
    } catch (x) {
      const f = apiFieldErrors(x);
      setErrors({
        note: f.fields.name ?? f.fields.note,
        expiry: f.fields.expires_in_days,
        scopes: f.fields.scopes,
        form: Object.keys(f.fields).length ? undefined : f.message,
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Link to="/settings/tokens" className={styles.back}>
        <ArrowLeftIcon size={16} /> Personal access tokens
      </Link>
      <PageHeader
        title="New personal access token (classic)"
        description="Personal access tokens function like ordinary OAuth access tokens. They can be used instead of a password for Git over HTTPS, or to authenticate to the API."
      />
      <form onSubmit={(e) => void submit(e)} noValidate aria-label="New personal access token">
        <FormStack wide>
          <FormStack>
            <Field label="Note" htmlFor={`${id}-note`} error={errors.note} hint="What’s this token for?">
              <Input
                id={`${id}-note`}
                ref={noteRef}
                value={note}
                maxLength={100}
                invalid={!!errors.note}
                onChange={(e) => {
                  setNote(e.target.value);
                  setErrors((x) => ({ ...x, note: undefined }));
                }}
                placeholder="e.g. laptop gh cli"
              />
            </Field>
            <Field label="Expiration" htmlFor={`${id}-exp`} error={errors.expiry}>
              <div className={styles.expiryRow}>
                <Select id={`${id}-exp`} value={expiry} onChange={(e) => setExpiry(e.target.value as ExpiryChoice)}>
                  {EXPIRY_OPTIONS.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </Select>
                {expiry === 'custom' && (
                  <Input
                    type="date"
                    aria-label="Custom expiration date"
                    value={custom}
                    min={dateInDays(1)}
                    max={dateInDays(3650)}
                    onChange={(e) => {
                      setCustom(e.target.value);
                      setErrors((x) => ({ ...x, expiry: undefined }));
                    }}
                  />
                )}
                {exp.days !== undefined && (
                  <span className={styles.expiryNote}>The token will expire on {formatDate(new Date(Date.now() + exp.days * 86_400_000).toISOString())}</span>
                )}
              </div>
            </Field>
            {expiry === 'none' && (
              <Banner tone="warning" icon={AlertIcon}>
                GitHub strongly recommends that you set an expiration date for your token to help keep your information secure.
              </Banner>
            )}
          </FormStack>
          <div>
            <h2 className={styles.reasonName} style={{ marginBottom: 4 }}>
              Select scopes
            </h2>
            <p className={styles.help} style={{ marginBottom: 10 }}>
              Scopes define the access for personal tokens. No scopes means read-only access to public information.
            </p>
            {errors.scopes && (
              <p role="alert" style={{ color: 'var(--danger)', marginBottom: 8 }}>
                {errors.scopes}
              </p>
            )}
            <ScopeTree value={scopes} onChange={setScopes} siteAdmin={siteAdmin} disabled={busy} />
          </div>
          {errors.form && (
            <Banner tone="danger" icon={AlertIcon}>
              {errors.form}
            </Banner>
          )}
          <ButtonRow>
            <Button type="submit" variant="success" loading={busy}>
              Generate token
            </Button>
            <Button onClick={() => navigate('/settings/tokens')}>Cancel</Button>
          </ButtonRow>
        </FormStack>
      </form>
    </>
  );
});
