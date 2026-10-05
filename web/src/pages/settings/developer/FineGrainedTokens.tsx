/**
 * Fine-grained personal access tokens on `/settings/tokens`: the list section
 * and the "New fine-grained personal access token" form
 * (`/settings/tokens/new?type=fine-grained`). Part of the TokenSettings chunk.
 */
import { useEffect, useId, useMemo, useRef, useState, type FormEvent } from 'react';
import { useResource, invalidate } from '../../../api/cache';
import {
  createFineGrainedToken,
  deleteFineGrainedToken,
  getPermissionCatalog,
  listFineGrainedTokens,
  listTokenOwners,
  type FgPerm,
  type FgPermCatalog,
  type FgSelection,
  type FgTokenOwner,
  type FineGrainedToken,
} from '../../../api/fineGrainedTokens';
import { apiFieldErrors, Banner, ButtonRow, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, Section } from '../../../components/settings/kit';
import { Link, navigate } from '../../../router';
import { Avatar } from '../../../ui/Badge';
import { Button, cx } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { AlertIcon, ArrowLeftIcon, ClockIcon, KeyIcon, PlusIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input, Select, Textarea } from '../../../ui/Input';
import appStyles from '../../apps/apps.module.css';
import { RepoAccessPicker, type PickedRepo } from '../../apps/RepoAccessPicker';
import { ListSkeleton, OneTimeSecret, lastUsedText } from './common';
import styles from './developer.module.css';
import {
  PERM_GROUPS,
  PERM_GROUP_TITLE,
  STATUS_LABEL,
  STATUS_TONE,
  allowedLevels,
  buildCreateBody,
  catalogLabels,
  defaultExpiry,
  emptyPerms,
  expiryError,
  expiryPresets,
  maxDaysFor,
  selectionText,
  summarizePermissions,
  validateFgForm,
  type FgFormErrors,
  type PermGroup,
  type PermValues,
} from './fineGrained';
import fg from './fineGrained.module.css';
import { expiryStatus, formatDate } from './logic';
import { useList } from './useList';

export const FG_LIST_KEY = 'dev:fg-tokens';
const OWNERS_KEY = 'dev:fg-owners';
const CATALOG_KEY = 'dev:fg-permissions';
export const NEW_FG_PATH = '/settings/tokens/new?type=fine-grained';

/** The fine-grained token just created, kept in memory until the list unmounts. */
let justCreated: FineGrainedToken | null = null;

// ------------------------------------------------------------------ list

export function FineGrainedSection() {
  const list = useList<FineGrainedToken>(FG_LIST_KEY, listFineGrainedTokens, { prepend: true });
  const catalog = useResource(CATALOG_KEY, getPermissionCatalog);
  const [created] = useState(() => justCreated);
  const { refresh } = list;
  useEffect(() => {
    justCreated = null;
    // Org admins approve / deny / revoke elsewhere: revalidate statuses on open.
    void refresh();
  }, [refresh]);
  const [confirm, setConfirm] = useState<FineGrainedToken | null>(null);
  const items = list.items && created && !list.items.some((t) => t.id === created.id) ? [created, ...list.items] : list.items;
  const labels = catalogLabels(catalog.data);
  return (
    <>
      {created?.token && (
        <div style={{ marginBottom: 16, display: 'flex', flexDirection: 'column', gap: 8 }}>
          <OneTimeSecret value={created.token} label="New fine-grained personal access token" warning="Make sure to copy your token now. You won’t be able to see it again!" />
          {created.status === 'pending' && (
            <Banner tone="warning" icon={ClockIcon}>
              This token is <strong>pending approval</strong> by the administrators of {created.resource_owner.login}. Until it is approved it can only read public
              resources.
            </Banner>
          )}
        </div>
      )}
      <Section
        title="Fine-grained tokens"
        description="Fine-grained tokens are scoped to one resource owner, a set of repositories and exactly the permissions you pick. They always expire."
        actions={
          <Button size="sm" variant="primary" leadingIcon={PlusIcon} onClick={() => navigate(NEW_FG_PATH)}>
            Generate new fine-grained token
          </Button>
        }
      >
        {items ? (
          <ItemList aria-label="Fine-grained personal access tokens" empty="You have no fine-grained personal access tokens yet.">
            {items.map((t) => (
              <FgRow key={t.id} t={t} fresh={t.id === created?.id} labels={labels} onDelete={() => setConfirm(t)} />
            ))}
          </ItemList>
        ) : list.error ? (
          <ItemList empty="Could not load your fine-grained tokens." />
        ) : (
          <ListSkeleton />
        )}
      </Section>
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Delete fine-grained token"
        confirmLabel="I understand, delete this token"
        onConfirm={() => {
          const t = confirm!;
          void list.remove(t.id, () => deleteFineGrainedToken(t.id), 'Token deleted');
        }}
      >
        <p>
          Any applications or scripts using <strong>{confirm?.name || 'this token'}</strong> will no longer be able to access {confirm?.resource_owner.login}’s
          resources. You cannot undo this action.
        </p>
      </ConfirmDialog>
    </>
  );
}

function FgRow({ t, fresh, labels, onDelete }: { t: FineGrainedToken; fresh: boolean; labels: (n: string) => string; onDelete: () => void }) {
  const exp = expiryStatus(t.expires_at);
  const summary = summarizePermissions(t.permissions, labels);
  return (
    <ItemRow
      icon={KeyIcon}
      className={cx(fresh && styles.highlightRow)}
      title={
        <>
          <span>{t.name || <em>Untitled token</em>}</span>
          <Pill tone={STATUS_TONE[t.status] ?? 'neutral'}>{STATUS_LABEL[t.status] ?? t.status}</Pill>
          {t.status === 'active' && exp.kind === 'expired' && <Pill tone="danger">Expired</Pill>}
          {t.status === 'active' && exp.kind === 'soon' && <Pill tone="warning">Expires soon</Pill>}
          {fresh && <Pill tone="accent">New</Pill>}
        </>
      }
      actions={
        <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={onDelete} aria-label={`Delete token ${t.name}`}>
          Delete
        </Button>
      }
    >
      {t.description && <div className={styles.help}>{t.description}</div>}
      <div className={styles.metaLines}>
        <span className={styles.metaInline}>
          <span className={fg.owner}>
            Resource owner: <Avatar user={{ login: t.resource_owner.login, avatarUrl: t.resource_owner.avatar_url }} size={16} square={t.resource_owner.type === 'Organization'} />
            <strong>{t.resource_owner.login}</strong>
          </span>
          <span>
            {t.repository_selection === 'selected' && t.repositories.length
              ? t.repositories.length <= 3
                ? t.repositories.map((r) => r.full_name).join(', ')
                : selectionText('selected', t.repositories.length)
              : selectionText(t.repository_selection, t.repositories.length, t.resource_owner.login)}
          </span>
        </span>
        <span>{summary.join(' · ')}</span>
        <span className={styles.metaInline}>
          <span>{lastUsedText(t.last_used_at)}</span>
          <span>
            {exp.kind === 'never' ? 'No expiration date' : exp.kind === 'expired' ? `Expired on ${formatDate(exp.at)}` : `Expires on ${formatDate(exp.at)}`}
          </span>
          <span className={styles.mono}>…{t.token_last_eight}</span>
        </span>
      </div>
    </ItemRow>
  );
}

// ------------------------------------------------------------------ new token

type Selection = { mode: FgSelection; repos: PickedRepo[] };

export function NewFineGrainedToken() {
  const id = useId();
  const owners = useResource(OWNERS_KEY, listTokenOwners);
  const catalog = useResource(CATALOG_KEY, getPermissionCatalog);
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [ownerLogin, setOwnerLogin] = useState<string | null>(null);
  const [preset, setPreset] = useState<string>('30');
  const [customDays, setCustomDays] = useState('30');
  const [selection, setSelection] = useState<Selection>({ mode: 'public', repos: [] });
  const [perms, setPerms] = useState<PermValues>(emptyPerms);
  const [reason, setReason] = useState('');
  const [errors, setErrors] = useState<FgFormErrors & { form?: string }>({});
  const [busy, setBusy] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  useEffect(() => nameRef.current?.focus(), []);

  const owner: FgTokenOwner | null = useMemo(() => {
    const all = owners.data ?? [];
    return all.find((o) => o.login === ownerLogin) ?? all.find((o) => o.fine_grained_allowed) ?? all[0] ?? null;
  }, [owners.data, ownerLogin]);
  const max = maxDaysFor(owner);
  const presets = expiryPresets(max);
  const effectivePreset = preset === 'custom' || presets.includes(Number(preset)) ? preset : String(defaultExpiry(max));
  const days = effectivePreset === 'custom' ? (customDays.trim() === '' ? null : Number(customDays)) : Number(effectivePreset);
  const isOrg = owner?.type === 'Organization';
  const form = { name, description, owner, expiresInDays: days, selection: selection.mode, repos: selection.repos, perms, reason };
  const labels = catalogLabels(catalog.data);
  const preview = owner ? buildCreateBody(form, catalog.data).permissions : null;
  // `metadata` (read) is always granted.
  const summary = preview ? summarizePermissions({ ...preview, repository: { metadata: 'read', ...preview.repository } }, labels) : [];

  const pickOwner = (login: string) => {
    setOwnerLogin(login);
    setSelection((s) => ({ ...s, repos: [] }));
    setPerms((p) => ({ ...p, organization: {} }));
    setErrors((e) => ({ ...e, owner: undefined, repositories: undefined, expiry: undefined }));
  };

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (busy) return;
    const errs = validateFgForm(form);
    setErrors(errs);
    if (errs.name) return nameRef.current?.focus();
    if (Object.keys(errs).length) return;
    setBusy(true);
    try {
      const t = await createFineGrainedToken(buildCreateBody(form, catalog.data));
      justCreated = t;
      invalidate(FG_LIST_KEY);
      navigate('/settings/tokens');
    } catch (x) {
      const f = apiFieldErrors(x);
      const k = f.fields;
      const mapped: FgFormErrors = {
        name: k.name,
        owner: k.resource_owner,
        expiry: k.expires_in_days,
        repositories: k.repository_ids ?? k.repositories ?? k.repository_selection,
        permissions: k.permissions,
      };
      const any = Object.values(mapped).some(Boolean);
      setErrors({ ...mapped, form: any ? undefined : f.message });
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
        title="New fine-grained personal access token"
        description="Create a fine-grained, repository-scoped token suitable for personal API use and for using Git over HTTPS."
      />
      <form onSubmit={(e) => void submit(e)} noValidate aria-label="New fine-grained personal access token">
        <FormStack wide>
          <FormStack>
            <Field label="Token name" htmlFor={`${id}-name`} error={errors.name} hint="A unique name for this token.">
              <Input
                id={`${id}-name`}
                ref={nameRef}
                value={name}
                maxLength={40}
                invalid={!!errors.name}
                onChange={(e) => {
                  setName(e.target.value);
                  setErrors((x) => ({ ...x, name: undefined }));
                }}
                placeholder="e.g. deploy bot"
              />
            </Field>
            <Field label="Description" htmlFor={`${id}-desc`}>
              <Textarea id={`${id}-desc`} value={description} rows={2} maxLength={1024} onChange={(e) => setDescription(e.target.value)} />
            </Field>
            <Field label="Resource owner" htmlFor={`${id}-owner`} error={errors.owner}>
              <div className={fg.ownerRow}>
                {owners.data ? (
                  <Select id={`${id}-owner`} value={owner?.login ?? ''} onChange={(e) => pickOwner(e.target.value)}>
                    {owners.data.map((o) => (
                      <option key={o.id} value={o.login} disabled={!o.fine_grained_allowed}>
                        {o.login}
                        {!o.fine_grained_allowed ? ' (fine-grained tokens not allowed)' : o.requires_approval ? ' (approval required)' : ''}
                      </option>
                    ))}
                  </Select>
                ) : owners.error ? (
                  <span role="alert">Could not load resource owners.</span>
                ) : (
                  <Skeleton width={220} height={32} />
                )}
                {owner && (
                  <span className={fg.ownerInfo} data-testid="owner-policy">
                    {owner.requires_approval ? (
                      <Pill tone="warning">Requires approval</Pill>
                    ) : (
                      <span>No approval required</span>
                    )}
                    <span>Maximum lifetime {max} days</span>
                  </span>
                )}
              </div>
            </Field>
            {owner?.requires_approval && (
              <Banner tone="info" icon={ClockIcon}>
                {owner.login} requires administrators to approve fine-grained tokens. The token will be <strong>pending</strong> until it is approved.
              </Banner>
            )}
            <Field label="Expiration" htmlFor={`${id}-exp`} error={errors.expiry}>
              <div className={styles.expiryRow}>
                <Select
                  id={`${id}-exp`}
                  value={effectivePreset}
                  onChange={(e) => {
                    setPreset(e.target.value);
                    setErrors((x) => ({ ...x, expiry: undefined }));
                  }}
                >
                  {presets.map((d) => (
                    <option key={d} value={String(d)}>
                      {d} days
                    </option>
                  ))}
                  <option value="custom">Custom…</option>
                </Select>
                {effectivePreset === 'custom' && (
                  <Input
                    type="number"
                    className={fg.daysInput}
                    aria-label="Days until expiration"
                    min={1}
                    max={max}
                    value={customDays}
                    invalid={!!errors.expiry}
                    onChange={(e) => {
                      setCustomDays(e.target.value);
                      setErrors((x) => ({ ...x, expiry: undefined }));
                    }}
                  />
                )}
                {days !== null && !expiryError(days, max) && (
                  <span className={styles.expiryNote}>The token will expire on {formatDate(new Date(Date.now() + days * 86_400_000).toISOString())}</span>
                )}
              </div>
            </Field>
          </FormStack>

          <div>
            <h2 className={fg.heading}>Repository access</h2>
            {errors.repositories && (
              <p role="alert" style={{ color: 'var(--danger)', margin: '0 0 8px' }}>
                {errors.repositories}
              </p>
            )}
            {owner ? (
              <RepoAccessPicker<Selection>
                key={owner.login}
                account={{ login: owner.login, id: owner.id, avatar_url: owner.avatar_url, type: owner.type }}
                value={selection}
                onChange={(v) => {
                  setSelection(v);
                  setErrors((x) => ({ ...x, repositories: undefined }));
                }}
                publicOption={{ label: 'Public repositories', description: 'Read-only access to public repositories.' }}
              />
            ) : (
              <Skeleton width="60%" />
            )}
          </div>

          <div>
            <h2 className={fg.heading}>Permissions</h2>
            <p className={styles.help} style={{ marginBottom: 10 }}>
              Choose the minimal permissions this token needs.
              {selection.mode === 'public' && ' Repository permissions are read-only for public repositories.'}
            </p>
            {errors.permissions && (
              <p role="alert" style={{ color: 'var(--danger)', marginBottom: 8 }}>
                {errors.permissions}
              </p>
            )}
            {catalog.data ? (
              <PermissionsEditor catalog={catalog.data} groups={PERM_GROUPS.filter((g) => g !== 'organization' || isOrg)} selection={selection.mode} value={perms} onChange={setPerms} />
            ) : catalog.error ? (
              <p role="alert">Could not load the permission list.</p>
            ) : (
              <FormStack>
                <Skeleton width="70%" />
                <Skeleton width="50%" />
              </FormStack>
            )}
          </div>

          {owner?.requires_approval && (
            <FormStack>
              <Field label="Reason for request" htmlFor={`${id}-reason`} hint={`Shown to the administrators of ${owner.login} when they review this token.`}>
                <Textarea id={`${id}-reason`} value={reason} rows={2} maxLength={1024} onChange={(e) => setReason(e.target.value)} />
              </Field>
            </FormStack>
          )}

          <section className={fg.summary} aria-label="Overview">
            <strong>Overview</strong>
            <ul>
              <li>{owner ? selectionText(selection.mode, selection.repos.length, owner.login) : 'No resource owner'}</li>
              {summary.map((s) => (
                <li key={s}>{s}</li>
              ))}
              <li>{days !== null && !expiryError(days, max) ? `Expires in ${days} day${days === 1 ? '' : 's'}` : 'Expiration not set'}</li>
            </ul>
          </section>

          {errors.form && (
            <Banner tone="danger" icon={AlertIcon}>
              {errors.form}
            </Banner>
          )}
          <ButtonRow>
            <Button type="submit" variant="success" loading={busy} disabled={!owner}>
              Generate token
            </Button>
            <Button onClick={() => navigate('/settings/tokens')}>Cancel</Button>
          </ButtonRow>
        </FormStack>
      </form>
    </>
  );
}

function PermissionsEditor({
  catalog,
  groups,
  selection,
  value,
  onChange,
}: {
  catalog: FgPermCatalog;
  groups: PermGroup[];
  selection: FgSelection;
  value: PermValues;
  onChange: (v: PermValues) => void;
}) {
  const id = useId();
  const row = (g: PermGroup, p: FgPerm) => {
    const fixed = g === 'repository' && p.name === 'metadata';
    const levels = allowedLevels(p.access, g, selection);
    const current = value[g][p.name] ?? '';
    const shown = current && !levels.includes(current) ? (levels.includes('read') ? 'read' : '') : current;
    const ctl = `${id}-${g}-${p.name}`;
    return (
      <div key={p.name} className={appStyles.permRow}>
        <label htmlFor={ctl} className={appStyles.permText}>
          <strong>{p.label}</strong>
          <span>{p.description}</span>
        </label>
        {fixed ? (
          <span className={fg.fixed} id={ctl}>
            Read-only (always granted)
          </span>
        ) : (
          <Select
            id={ctl}
            aria-label={g === 'repository' ? p.label : `${p.label} (${g})`}
            value={shown}
            onChange={(e) => onChange({ ...value, [g]: { ...value[g], [p.name]: e.target.value as '' | 'read' | 'write' } })}
          >
            <option value="">No access</option>
            {levels.map((l) => (
              <option key={l} value={l}>
                {l === 'read' ? 'Read-only' : 'Read and write'}
              </option>
            ))}
          </Select>
        )}
      </div>
    );
  };
  return (
    <div className={appStyles.permGroups}>
      {groups.map((g) => (
        <fieldset key={g} className={appStyles.permGroup}>
          <legend>{PERM_GROUP_TITLE[g]}</legend>
          {catalog[g].length ? catalog[g].map((p) => row(g, p)) : <p className={appStyles.hint} style={{ padding: '8px 14px', margin: 0 }}>No permissions in this group.</p>}
        </fieldset>
      ))}
    </div>
  );
}
