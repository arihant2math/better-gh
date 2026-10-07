import { observer } from 'mobx-react-lite';
import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { invalidate, useResource } from '../../api/cache';
import { createRepo, generateRepo, getOrg, listAccessibleRepos, listGitignoreTemplates, listLicenses, listOrgTeams, profileKeys, repoExists, type RestRepo } from '../../api/profile';
import { session } from '../../app/session';
import { site, visibilityPolicy } from '../../app/site';
import { apiFieldErrors, Banner, ButtonRow, Checkbox, FormStack, PageHeader, RadioCards, Section, useDebounced, type FieldErrors } from '../../components/settings/kit';
import { getBoot } from '../../boot';
import { Link, navigate, useQuery } from '../../router';
import { formatKeys } from '../../shortcuts/manager';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { hasSync, sync } from '../../sync';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { AlertIcon, CheckIcon, ChevronDownIcon, GlobeIcon, LockIcon, OrganizationIcon, XIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { Menu } from '../../ui/Menu';
import { Spinner } from '../../ui/Spinner';
import { GITIGNORE_TEMPLATES, LICENSE_TEMPLATES, normalizeRepoName, repoNameError } from './names';
import styles from './New.module.css';
import { useOwners } from './owners';

type Visibility = 'public' | 'private' | 'internal';
type Availability = { key: string; state: 'checking' | 'available' | 'taken' | 'error' } | null;

/** `/new`: create a repository (optionally from a template). */
export default observer(function NewRepoPage() {
  const query = useQuery();
  const owners = useOwners();
  const me = session.user;
  const [ownerLogin, setOwnerLogin] = useState(() => {
    const q = query.get('owner');
    return (q && owners.find((o) => o.login.toLowerCase() === q.toLowerCase())?.login) || me?.login || '';
  });
  const owner = owners.find((o) => o.login === ownerLogin) ?? owners[0];
  const [rawName, setRawName] = useState('');
  const [description, setDescription] = useState('');
  const [chosenVisibility, setVisibility] = useState<Visibility | null>(null);
  const [readme, setReadme] = useState(false);
  const [gitignore, setGitignore] = useState('');
  const [license, setLicense] = useState('');
  const [teamId, setTeamId] = useState('');
  const [includeAllBranches, setIncludeAllBranches] = useState(false);
  const [template, setTemplate] = useState(() => {
    const o = query.get('template_owner');
    const n = query.get('template_name');
    return o && n ? `${o}/${n}` : '';
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [touched, setTouched] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  const formRef = useRef<HTMLFormElement>(null);
  const ownerBtn = useRef<HTMLButtonElement>(null);
  const [ownerMenu, setOwnerMenu] = useState(false);
  const ids = { name: useId(), desc: useId(), template: useId(), gitignore: useId(), license: useId(), team: useId() };

  // Templates the viewer can see (`is_template` on /user/repos).
  const accessible = useResource('profile:accessible-repos', listAccessibleRepos);
  const templates = useMemo(() => {
    const list = (accessible.data ?? []).filter((r) => r.is_template);
    if (template && !list.some((r) => r.full_name.toLowerCase() === template.toLowerCase())) {
      const [o, n] = template.split('/');
      list.unshift({ full_name: template, name: n!, owner: { login: o! } } as RestRepo);
    }
    return list;
  }, [accessible.data, template]);

  // Site policy (allowed visibilities, default). "Internal" only exists for
  // organizations (and not for template generation).
  const { allowed, preferred } = visibilityPolicy(site.info, !!owner?.isOrg && !template);
  const picked = chosenVisibility ?? preferred;
  const visibility: Visibility = allowed.includes(picked)
    ? picked
    : picked === 'internal' && allowed.includes('private')
      ? 'private'
      : preferred;

  // Organization policy for members (owners can always create).
  const orgPolicy = useResource(owner?.isOrg && owner.role !== 'admin' ? profileKeys.org(owner.login) : null, () => getOrg(owner!.login));
  const policy = orgPolicy.data;
  const memberBlocked = !!policy && policy.members_can_create_repositories === false;
  const visibilityBlocked =
    !!policy &&
    ((visibility === 'public' && policy.members_can_create_public_repositories === false) ||
      (visibility === 'internal' && policy.members_can_create_internal_repositories === false) ||
      (visibility === 'private' && policy.members_can_create_private_repositories === false));


  // Template lists (loaded once the "Initialize" section shows; the built-in lists cover the wait).
  const initOpen = !template;
  const gitignoreList = useResource(initOpen ? profileKeys.gitignoreTemplates : null, listGitignoreTemplates, { immutable: true });
  const licenseList = useResource(initOpen ? profileKeys.licenses : null, listLicenses, { immutable: true });
  const gitignoreOptions = gitignoreList.data ?? GITIGNORE_TEMPLATES;
  const licenseOptions = useMemo(() => licenseList.data?.map((l) => ({ id: l.key, label: l.name })) ?? LICENSE_TEMPLATES, [licenseList.data]);
  const gitignoreValue = gitignoreOptions.includes(gitignore) ? gitignore : '';
  const licenseValue = licenseOptions.some((l) => l.id === license) ? license : '';

  // Organization owners can grant a team access right away (`team_id`).
  const teamsList = useResource(initOpen && owner?.isOrg && owner.role === 'admin' ? profileKeys.teams(owner.login) : null, () => listOrgTeams(owner!.login));
  const teams = owner?.isOrg && owner.role === 'admin' ? (teamsList.data ?? []) : [];
  const teamValue = teams.some((t) => String(t.id) === teamId) ? teamId : '';

  const name = normalizeRepoName(rawName);
  const nameErr = repoNameError(name);
  const normalized = rawName.trim() !== '' && name !== rawName.trim();

  // Live availability check (debounced): 404 → available, 200 → taken.
  const checkKey = owner && !nameErr ? `${owner.login}/${name}`.toLowerCase() : '';
  const debouncedKey = useDebounced(checkKey, 300);
  const [avail, setAvail] = useState<Availability>(null);
  useEffect(() => {
    if (!debouncedKey) return;
    const [o, n] = debouncedKey.split('/');
    const ctl = new AbortController();
    setAvail({ key: debouncedKey, state: 'checking' });
    repoExists(o!, n!, ctl.signal).then(
      (exists) => setAvail({ key: debouncedKey, state: exists ? 'taken' : 'available' }),
      () => !ctl.signal.aborted && setAvail({ key: debouncedKey, state: 'error' }),
    );
    return () => ctl.abort();
  }, [debouncedKey]);
  const availability = avail && avail.key === checkKey ? avail.state : checkKey ? 'checking' : null;

  const clientNameError = !touched && !rawName ? null : rawName.trim() === '' ? 'Repository name is required' : nameErr;
  const nameError = errors.name ?? clientNameError ?? (availability === 'taken' ? `The repository ${name} already exists on this account.` : null);

  const submit = async () => {
    setTouched(true);
    if (busy || !owner) return;
    if (nameErr || availability === 'taken') {
      nameRef.current?.focus();
      return;
    }
    setBusy(true);
    setErrors({});
    setFormError(null);
    try {
      let repo: RestRepo;
      if (template) {
        const [to, tn] = template.split('/');
        repo = await generateRepo(to!, tn!, {
          owner: owner.login,
          name,
          description: description.trim() || undefined,
          private: visibility !== 'public',
          include_all_branches: includeAllBranches,
        });
      } else {
        repo = await createRepo(owner.isOrg ? owner.login : null, {
          name,
          description: description.trim() || undefined,
          visibility,
          auto_init: readme,
          gitignore_template: gitignoreValue || undefined,
          license_template: licenseValue || undefined,
          team_id: teamValue ? Number(teamValue) : undefined,
        });
      }
      invalidate('profile:repos:');
      invalidate('profile:org-repos:');
      invalidate('profile:accessible-repos');
      // Subscribe to the new repository so the code page renders from the store.
      if (hasSync()) await Promise.race([sync().ensureScope(`repo:${repo.id}`).catch(() => false), new Promise((r) => setTimeout(r, 1500))]);
      navigate(`/${repo.owner?.login ?? owner.login}/${repo.name}`);
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setErrors(fields);
      if (fields.name) nameRef.current?.focus();
      else setFormError(message);
      setBusy(false);
    }
  };

  useShortcuts('New repository', {
    'mod+enter': { handler: () => void submit(), description: 'Create repository', group: 'Forms', allowInInput: true },
  });

  if (!me) return null;
  const canSubmit = !busy && !!owner && !!rawName.trim() && !nameErr && availability !== 'taken' && !memberBlocked && !visibilityBlocked;
  const visibilityOptions = [
    { value: 'public' as const, label: 'Public', icon: GlobeIcon, description: 'Anyone who can reach this site can see this repository. You choose who can commit.' },
    {
      value: 'internal' as const,
      label: 'Internal',
      icon: OrganizationIcon,
      description: `Everyone signed in to ${getBoot().config.siteName} can see this repository. You choose who can commit.`,
    },
    { value: 'private' as const, label: 'Private', icon: LockIcon, description: 'You choose who can see and commit to this repository.' },
  ].filter((o) => allowed.includes(o.value));

  return (
    <div className={styles.page}>
      <PageHeader
        title="Create a new repository"
        description={
          <>
            A repository contains all project files, including the revision history. Already have a project repository elsewhere?{' '}
            <Link to="/new/import">Import a repository</Link>.
          </>
        }
      />
      <form
        className={styles.form}
        ref={formRef}
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <FormStack wide>
          <p className={styles.required}>
            Required fields are marked with an asterisk (<span aria-hidden>*</span>).
          </p>
          <Field
            label="Repository template"
            htmlFor={ids.template}
            hint={template ? 'Start your repository with the template’s files.' : 'Start your repository with a template repository’s contents.'}
          >
            <Select id={ids.template} value={template} onChange={(e) => setTemplate(e.target.value)}>
              <option value="">No template</option>
              {templates.map((r) => (
                <option key={r.full_name} value={r.full_name}>
                  {r.full_name}
                </option>
              ))}
            </Select>
          </Field>
          {template && (
            <Checkbox
              checked={includeAllBranches}
              onChange={setIncludeAllBranches}
              label="Include all branches"
              description="Copy all branches from the template, not just the default branch."
            />
          )}

          <div className={styles.ownerName}>
            <div className={styles.ownerCol}>
              <span className={styles.label} id={`${ids.name}-owner`}>
                Owner <span aria-hidden>*</span>
              </span>
              <button
                ref={ownerBtn}
                type="button"
                className={styles.ownerButton}
                aria-haspopup="menu"
                aria-expanded={ownerMenu}
                aria-labelledby={`${ids.name}-owner ${ids.name}-owner-value`}
                onClick={() => setOwnerMenu((v) => !v)}
              >
                <Avatar user={owner} size={20} square={owner?.isOrg} />
                <span id={`${ids.name}-owner-value`}>{owner?.login}</span>
                <ChevronDownIcon size={16} />
              </button>
              <Menu
                open={ownerMenu}
                onClose={() => setOwnerMenu(false)}
                anchor={ownerBtn}
                aria-label="Choose an owner"
                items={owners.map((o) => ({
                  id: o.login,
                  text: o.login,
                  label: o.login,
                  description: o.isOrg ? (o.role === 'admin' ? 'Organization · owner' : 'Organization · member') : 'Your personal account',
                  leading: <Avatar user={o} size={20} square={o.isOrg} />,
                  trailing: o.login === owner?.login ? <CheckIcon size={16} /> : undefined,
                  onSelect: () => {
                    setOwnerLogin(o.login);
                    setErrors({});
                    nameRef.current?.focus();
                  },
                }))}
              />
            </div>
            <span className={styles.slash} aria-hidden>
              /
            </span>
            <div className={styles.nameCol}>
              <label className={styles.label} htmlFor={ids.name}>
                Repository name <span aria-hidden>*</span>
              </label>
              <Input
                ref={nameRef}
                id={ids.name}
                value={rawName}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                required
                maxLength={100}
                invalid={!!nameError}
                aria-describedby={`${ids.name}-status`}
                onChange={(e) => {
                  setRawName(e.target.value);
                  if (errors.name) setErrors((x) => ({ ...x, name: undefined }));
                }}
                onBlur={() => rawName && setTouched(true)}
                trailing={
                  availability === 'checking' ? (
                    <Spinner size={14} />
                  ) : availability === 'available' && !nameError ? (
                    <CheckIcon size={16} className={styles.ok} aria-label="Available" />
                  ) : nameError ? (
                    <XIcon size={16} className={styles.bad} aria-label="Unavailable" />
                  ) : null
                }
              />
            </div>
          </div>
          <div id={`${ids.name}-status`} className={styles.nameStatus} aria-live="polite">
            {nameError ? (
              <span className={styles.bad}>
                <AlertIcon size={14} /> {nameError}
              </span>
            ) : availability === 'available' ? (
              <span className={styles.ok}>
                <CheckIcon size={14} /> {name} is available.
              </span>
            ) : null}
            {normalized && !nameErr && (
              <span className={styles.hint}>
                Your new repository will be created as <strong>{name}</strong>.
              </span>
            )}
            {!rawName && <span className={styles.hint}>Great repository names are short and memorable.</span>}
          </div>

          <Field label="Description (optional)" htmlFor={ids.desc} error={errors.description}>
            <Textarea id={ids.desc} rows={2} value={description} maxLength={350} onChange={(e) => setDescription(e.target.value)} />
          </Field>

          {memberBlocked && (
            <Banner tone="warning" icon={AlertIcon}>
              Members of {owner?.login} can’t create repositories. Ask an organization owner, or choose another owner.
            </Banner>
          )}
        </FormStack>

        <Section title="Visibility">
          <RadioCards aria-label="Visibility" value={visibility} onChange={setVisibility} options={visibilityOptions} />
          {visibilityBlocked && !memberBlocked && (
            <div className={styles.inlineWarning}>
              <Banner tone="warning" icon={AlertIcon}>
                Members of {owner?.login} can’t create {visibility} repositories.
              </Banner>
            </div>
          )}
        </Section>

        {!template && (
          <Section title="Initialize this repository with">
            <FormStack wide>
              <Checkbox checked={readme} onChange={setReadme} label="Add a README file" description="This is where you can write a long description for your project." />
              <div className={styles.templates}>
                <Field label="Add .gitignore" htmlFor={ids.gitignore} hint="Choose which files not to track from a list of templates." error={errors.gitignore_template}>
                  <Select id={ids.gitignore} value={gitignoreValue} onChange={(e) => setGitignore(e.target.value)}>
                    <option value="">.gitignore template: None</option>
                    {gitignoreOptions.map((t) => (
                      <option key={t} value={t}>
                        {t}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Choose a license" htmlFor={ids.license} hint="A license tells others what they can and can’t do with your code." error={errors.license_template}>
                  <Select id={ids.license} value={licenseValue} onChange={(e) => setLicense(e.target.value)}>
                    <option value="">License: None</option>
                    {licenseOptions.map((l) => (
                      <option key={l.id} value={l.id}>
                        {l.label}
                      </option>
                    ))}
                  </Select>
                </Field>
              </div>
              {teams.length > 0 && (
                <Field label="Grant access to a team (optional)" htmlFor={ids.team} hint="The team gets read access; change it later in the repository settings." error={errors.team_id}>
                  <Select id={ids.team} value={teamValue} onChange={(e) => setTeamId(e.target.value)}>
                    <option value="">No team</option>
                    {teams.map((t) => (
                      <option key={t.id} value={String(t.id)}>
                        {t.name}
                      </option>
                    ))}
                  </Select>
                </Field>
              )}
            </FormStack>
          </Section>
        )}

        <div className={styles.footer}>
          {formError && (
            <Banner tone="danger" icon={AlertIcon}>
              {formError}
            </Banner>
          )}
          <p className={styles.summary}>
            You are creating a {visibility} repository{' '}
            {owner?.isOrg ? (
              <>
                in the <strong>{owner.login}</strong> organization.
              </>
            ) : (
              'in your personal account.'
            )}
          </p>
          <ButtonRow end>
            <Button type="submit" variant="primary" loading={busy} disabled={!canSubmit} kbd={formatKeys('mod+enter').join('')}>
              Create repository
            </Button>
          </ButtonRow>
        </div>
      </form>
    </div>
  );
});
