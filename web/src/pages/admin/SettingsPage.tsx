import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { mutate, refresh, useResource } from '../../api/cache';
import { site } from '../../app/site';
import styles from '../../components/admin/admin.module.css';
import { ErrorState, PageHeader, Panel, errorMessage, useConfirm } from '../../components/admin/kit';
import { formatKeys } from '../../shortcuts/manager';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon, DotFillIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { getSamlInfo, getSettings, patchSettings, SAML_INFO_KEY, type SiteSettings } from './api';
import {
  SECTIONS,
  dirtySections,
  sectionOf,
  sectionTitle,
  toForm,
  toPatch,
  validate,
  type SectionKey,
  type SettingsForm,
} from './settingsForm';
import {
  ActionsSection,
  AnnouncementSection,
  AuthSection,
  GitSection,
  MaintenanceSection,
  OrganizationsSection,
  PrivacySection,
  RateLimitsSection,
  RepositoriesSection,
  RetentionSection,
  MarkdownSection,
  SignupSection,
  SmtpSection,
} from './settingsSections';
import s from './settings.module.css';
import { settingsDirty } from './settingsState';

const KEY = 'admin:settings';

const anchorOf = (k: SectionKey) => SECTIONS.find((x) => x.key === k)!.anchor;

function scrollToSection(k: SectionKey) {
  document.getElementById(anchorOf(k))?.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

/** Section of a 422 field error (`smtp`, `auth_providers.oidc.name`, …). */
function serverSection(err: unknown): SectionKey | null {
  if (!(err instanceof ApiError)) return null;
  const field = (err.body as { errors?: { field?: string }[] } | null)?.errors?.[0]?.field;
  if (!field) return null;
  return SECTIONS.find((x) => field === x.key || field.startsWith(`${x.key}.`))?.key ?? null;
}

export default function SettingsPage() {
  const res = useResource(KEY, getSettings);
  const [base, setBase] = useState<SiteSettings | null>(null);
  const [saved, setSaved] = useState<SettingsForm | null>(null);
  const [draft, setDraft] = useState<SettingsForm | null>(null);
  const [saving, setSaving] = useState(false);
  const [serverError, setServerError] = useState<{ section: SectionKey | null; message: string } | null>(null);
  const [active, setActive] = useState<SectionKey>('signup');
  const confirm = useConfirm();

  const dirty = draft && saved ? dirtySections(draft, saved) : [];
  const isDirty = dirty.length > 0;

  // Adopt fresh server data whenever there are no local edits to protect.
  if (res.data && res.data !== base && !isDirty) {
    const f = toForm(res.data);
    setBase(res.data);
    setSaved(f);
    setDraft(f);
  }

  const errors = draft ? validate(draft) : {};

  // Admin nav dot + leave-page warning while there are unsaved edits.
  useEffect(() => {
    settingsDirty.set(isDirty);
    if (!isDirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onBeforeUnload);
    return () => window.removeEventListener('beforeunload', onBeforeUnload);
  }, [isDirty]);
  useEffect(() => () => settingsDirty.set(false), []);

  // Jump to `#section` once loaded; track the section in view for the index.
  const loaded = !!draft;
  const scrolled = useRef(false);
  useEffect(() => {
    if (!loaded) return;
    if (!scrolled.current) {
      scrolled.current = true;
      const hash = location.hash.slice(1);
      const sec = SECTIONS.find((x) => x.anchor === hash);
      if (sec) document.getElementById(sec.anchor)?.scrollIntoView({ block: 'start' });
    }
    const io = new IntersectionObserver(
      (entries) => {
        const top = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        const sec = top && SECTIONS.find((x) => x.anchor === top.target.id);
        if (sec) setActive(sec.key);
      },
      { rootMargin: '0px 0px -65% 0px' },
    );
    for (const sec of SECTIONS) {
      const el = document.getElementById(sec.anchor);
      if (el) io.observe(el);
    }
    return () => io.disconnect();
  }, [loaded]);

  const update =
    <K extends SectionKey>(k: K) =>
    (patch: Partial<SettingsForm[K]>) => {
      setDraft((d) => (d ? { ...d, [k]: { ...d[k], ...patch } } : d));
      setServerError((e) => (e && (e.section === k || e.section === null) ? null : e));
    };

  const revert = (k: SectionKey) => {
    if (!saved) return;
    setDraft((d) => (d ? { ...d, [k]: saved[k] } : d));
    setServerError((e) => (e?.section === k ? null : e));
  };

  const discard = () => {
    setDraft(saved);
    setServerError(null);
  };

  const save = async () => {
    if (!draft || !isDirty || saving) return;
    const blocking = Object.keys(errors).filter((k) => dirty.includes(sectionOf(k)));
    if (blocking.length) {
      scrollToSection(sectionOf(blocking[0]!));
      toast({ kind: 'error', title: 'Fix the highlighted fields first', description: errors[blocking[0]!] });
      return;
    }
    setSaving(true);
    setServerError(null);
    try {
      const next = await patchSettings(toPatch(draft, dirty));
      mutate<SiteSettings>(KEY, () => next);
      const f = toForm(next);
      setBase(next);
      setSaved(f);
      setDraft(f);
      void site.refresh();
      // The SAML service provider panel shows the saved configuration.
      if (dirty.includes('auth_providers')) void refresh(SAML_INFO_KEY, getSamlInfo).catch(() => undefined);
      toast({ kind: 'success', title: 'Settings saved', description: dirty.map(sectionTitle).join(', ') });
    } catch (err) {
      const section = serverSection(err);
      setServerError({ section, message: errorMessage(err) });
      if (section) scrollToSection(section);
    } finally {
      setSaving(false);
    }
  };

  useShortcuts('Site settings', {
    'mod+s': {
      handler: () => {
        void save();
      },
      description: 'Save settings',
      group: 'Site admin',
      allowInInput: true,
    },
  });

  const enableMaintenance = () =>
    confirm({
      title: 'Turn on maintenance mode?',
      body: (
        <>
          While maintenance mode is on, <strong>everyone except site administrators</strong> is locked out: the API, git and the web app answer
          with “503 Service Unavailable”. It takes effect when you save.
        </>
      ),
      confirmLabel: 'Turn on maintenance mode',
      danger: true,
      onConfirm: async () => update('maintenance')({ enabled: true }),
    });

  const renderSection = (k: SectionKey): ReactNode => {
    if (!draft) return null;
    switch (k) {
      case 'signup':
        return <SignupSection value={draft.signup} onChange={update('signup')} errors={errors} />;
      case 'repositories':
        return <RepositoriesSection value={draft.repositories} onChange={update('repositories')} errors={errors} />;
      case 'privacy':
        return <PrivacySection value={draft.privacy} onChange={update('privacy')} errors={errors} />;
      case 'organizations':
        return <OrganizationsSection value={draft.organizations} onChange={update('organizations')} errors={errors} />;
      case 'announcement':
        return <AnnouncementSection value={draft.announcement} onChange={update('announcement')} errors={errors} />;
      case 'rate_limits':
        return <RateLimitsSection value={draft.rate_limits} onChange={update('rate_limits')} errors={errors} />;
      case 'auth_providers':
        return <AuthSection value={draft.auth_providers} onChange={update('auth_providers')} errors={errors} />;
      case 'smtp':
        return <SmtpSection value={draft.smtp} onChange={update('smtp')} errors={errors} />;
      case 'git':
        return <GitSection value={draft.git} onChange={update('git')} errors={errors} />;
      case 'retention':
        return <RetentionSection value={draft.retention} onChange={update('retention')} errors={errors} />;
      case 'maintenance':
        return <MaintenanceSection value={draft.maintenance} onChange={update('maintenance')} errors={errors} onEnable={enableMaintenance} />;
      case 'actions':
        return <ActionsSection value={draft.actions} onChange={update('actions')} errors={errors} />;
      case 'markdown':
        return <MarkdownSection value={draft.markdown} onChange={update('markdown')} errors={errors} />;
    }
  };

  const errorSections = new Set(Object.keys(errors).map(sectionOf));
  if (serverError?.section) errorSections.add(serverError.section);

  return (
    <div className={styles.page}>
      <PageHeader title="Site settings" description="Instance-wide policies for every user. Changes apply as soon as you save them." />
      {!draft ? (
        res.error ? (
          <ErrorState error={res.error} onRetry={() => void refresh(KEY, getSettings).catch(() => undefined)} />
        ) : (
          <div className={styles.stack} aria-busy="true">
            {SECTIONS.slice(0, 4).map((sec) => (
              <Panel key={sec.key} title={sec.title}>
                <Skeleton width="40%" />
                <div style={{ height: 10 }} />
                <Skeleton width="75%" />
              </Panel>
            ))}
          </div>
        )
      ) : (
        <div className={s.layout}>
          <div className={styles.stack}>
            {SECTIONS.map((sec) => {
              const isSecDirty = dirty.includes(sec.key);
              return (
                <Panel
                  key={sec.key}
                  id={sec.anchor}
                  className={s.section}
                  danger={sec.key === 'maintenance' && draft.maintenance.enabled}
                  title={
                    <>
                      {sec.title}{' '}
                      {isSecDirty && (
                        <span className={s.dirtyTag}>
                          <DotFillIcon size={10} /> Unsaved
                        </span>
                      )}
                    </>
                  }
                  actions={
                    isSecDirty ? (
                      <Button size="sm" variant="ghost" onClick={() => revert(sec.key)}>
                        Revert
                      </Button>
                    ) : undefined
                  }
                >
                  {serverError && serverError.section === sec.key && (
                    <div className={styles.formError} role="alert" style={{ marginBottom: 14 }}>
                      <AlertIcon size={14} /> {serverError.message}
                    </div>
                  )}
                  {renderSection(sec.key)}
                </Panel>
              );
            })}
            {(isDirty || serverError) && (
              <div className={styles.saveBar} role="region" aria-label="Unsaved changes">
                <span>
                  {serverError && !serverError.section ? (
                    <span style={{ color: 'var(--danger)' }}>
                      <AlertIcon size={14} /> {serverError.message}
                    </span>
                  ) : isDirty ? (
                    <>
                      Unsaved changes in <span className={s.saveList}>{dirty.map(sectionTitle).join(', ')}</span>
                    </>
                  ) : (
                    'Could not save.'
                  )}
                </span>
                <Button onClick={discard} disabled={!isDirty || saving}>
                  Discard
                </Button>
                <Button variant="primary" loading={saving} disabled={!isDirty} kbd={formatKeys('mod+s').join(' ')} onClick={() => void save()}>
                  Save changes
                </Button>
              </div>
            )}
          </div>
          <nav className={s.indexWrap} aria-label="Settings sections">
            <ul className={s.index}>
              <li className={s.indexTitle}>On this page</li>
              {SECTIONS.map((sec) => {
                const mark = errorSections.has(sec.key) && dirty.includes(sec.key) ? 'error' : dirty.includes(sec.key) ? 'dirty' : null;
                return (
                  <li key={sec.key}>
                    <a
                      href={`#${sec.anchor}`}
                      className={s.indexLink}
                      aria-current={active === sec.key ? 'true' : undefined}
                      onClick={(e) => {
                        e.preventDefault();
                        setActive(sec.key);
                        history.replaceState(history.state, '', `#${sec.anchor}`);
                        scrollToSection(sec.key);
                      }}
                    >
                      {sec.title}
                      {mark && (
                        <span className={s.indexMark} data-kind={mark} aria-label={mark === 'error' ? 'Has errors' : 'Unsaved changes'}>
                          {mark === 'error' ? <AlertIcon size={12} /> : <DotFillIcon size={10} />}
                        </span>
                      )}
                    </a>
                  </li>
                );
              })}
            </ul>
          </nav>
        </div>
      )}
      {confirm.dialog}
    </div>
  );
}
