import { observer } from 'mobx-react-lite';
import { useEffect, useId, useRef, useState } from 'react';
import { invalidate } from '../../api/cache';
import { errorMessage, fieldErrors, type FieldErrorLabels } from '../../api/errors';
import { accountExists, createOrg } from '../../api/profile';
import { session } from '../../app/session';
import { Banner, ButtonRow, FormStack, PageHeader, RadioCards, Section, useDebounced, type FieldErrors } from '../../components/settings/kit';
import { navigate } from '../../router';
import { formatKeys } from '../../shortcuts/manager';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { hasSync, sync } from '../../sync';
import { Button } from '../../ui/Button';
import { AlertIcon, CheckIcon, PersonIcon, XIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { Spinner } from '../../ui/Spinner';
import { emailError, loginError } from './names';
import styles from './New.module.css';

type Availability = { key: string; state: 'checking' | 'available' | 'taken' | 'error' } | null;

/** Friendlier wording for the login field's 422 codes. */
const LOGIN_LABELS: FieldErrorLabels = {
  login: { already_exists: 'This name is already taken', missing_field: 'Organization name is required', invalid: 'Organization name is invalid or reserved' },
};

/** `/organizations/new`: create an organization owned by the viewer. */
export default observer(function NewOrgPage() {
  const me = session.user;
  const [login, setLogin] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [email, setEmail] = useState('');
  const [touched, setTouched] = useState<Record<string, boolean>>({});
  const [errors, setErrors] = useState<FieldErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const loginRef = useRef<HTMLInputElement>(null);
  const emailRef = useRef<HTMLInputElement>(null);
  const ids = { login: useId(), name: useId(), email: useId() };

  const trimmed = login.trim();
  const loginErr = loginError(trimmed);
  const checkKey = loginErr ? '' : trimmed.toLowerCase();
  const debounced = useDebounced(checkKey, 300);
  const [avail, setAvail] = useState<Availability>(null);
  useEffect(() => {
    if (!debounced) return;
    const ctl = new AbortController();
    setAvail({ key: debounced, state: 'checking' });
    accountExists(debounced, ctl.signal).then(
      (exists) => setAvail({ key: debounced, state: exists ? 'taken' : 'available' }),
      () => !ctl.signal.aborted && setAvail({ key: debounced, state: 'error' }),
    );
    return () => ctl.abort();
  }, [debounced]);
  const availability = avail && avail.key === checkKey ? avail.state : checkKey ? 'checking' : null;

  const loginMsg = errors.login ?? (touched.login || login ? (trimmed ? loginErr : 'Organization name is required') : null) ?? (availability === 'taken' ? `The name ${trimmed} is already taken.` : null);
  const emailMsg = errors.billing_email ?? errors.email ?? (touched.email ? emailError(email.trim()) : null);

  const submit = async () => {
    setTouched({ login: true, email: true });
    if (busy) return;
    if (loginErr || availability === 'taken') {
      loginRef.current?.focus();
      return;
    }
    if (emailError(email.trim())) {
      emailRef.current?.focus();
      return;
    }
    setBusy(true);
    setErrors({});
    setFormError(null);
    try {
      const org = await createOrg({ login: trimmed, name: displayName.trim() || undefined, billing_email: email.trim() });
      invalidate('profile:orgs:');
      if (hasSync()) await Promise.race([sync().ensureScope(`org:${org.id}`).catch(() => false), new Promise((r) => setTimeout(r, 1500))]);
      navigate(`/${org.login}`);
    } catch (e) {
      const f = fieldErrors(e, LOGIN_LABELS);
      setErrors(f);
      if (f.login) loginRef.current?.focus();
      else if (f.billing_email) emailRef.current?.focus();
      else setFormError(errorMessage(e));
      setBusy(false);
    }
  };

  useShortcuts('New organization', {
    'mod+enter': { handler: () => void submit(), description: 'Create organization', group: 'Forms', allowInInput: true },
  });

  if (!me) return null;
  const canSubmit = !busy && !!trimmed && !loginErr && availability !== 'taken';

  return (
    <div className={styles.page}>
      <PageHeader title="Set up your organization" description="Organizations are shared accounts where people collaborate across many repositories at once." />
      <form
        className={styles.form}
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <FormStack wide>
          <div>
            <label className={styles.label} htmlFor={ids.login}>
              Organization name <span aria-hidden>*</span>
            </label>
            <Input
              ref={loginRef}
              id={ids.login}
              value={login}
              autoFocus
              autoComplete="off"
              spellCheck={false}
              maxLength={39}
              required
              invalid={!!loginMsg}
              aria-describedby={`${ids.login}-status`}
              onChange={(e) => {
                setLogin(e.target.value);
                if (errors.login) setErrors((x) => ({ ...x, login: undefined }));
              }}
              onBlur={() => login && setTouched((t) => ({ ...t, login: true }))}
              trailing={
                availability === 'checking' ? (
                  <Spinner size={14} />
                ) : availability === 'available' && !loginMsg ? (
                  <CheckIcon size={16} className={styles.ok} aria-label="Available" />
                ) : loginMsg ? (
                  <XIcon size={16} className={styles.bad} aria-label="Unavailable" />
                ) : null
              }
            />
            <div id={`${ids.login}-status`} className={styles.nameStatus} style={{ marginTop: 6 }} aria-live="polite">
              {loginMsg ? (
                <span className={styles.bad}>
                  <AlertIcon size={14} /> {loginMsg}
                </span>
              ) : availability === 'available' ? (
                <span className={styles.ok}>
                  <CheckIcon size={14} /> {trimmed} is available.
                </span>
              ) : null}
              <span className={styles.hint}>
                This will be the name of your organization’s account. Its URL will be{' '}
                <strong>
                  {location.origin}/{trimmed || 'name'}
                </strong>
                .
              </span>
            </div>
          </div>

          <Field label="Display name (optional)" htmlFor={ids.name} hint="Shown on the organization’s profile instead of its login." error={errors.name}>
            <Input id={ids.name} value={displayName} maxLength={255} onChange={(e) => setDisplayName(e.target.value)} />
          </Field>

          <Field label="Contact email *" htmlFor={ids.email} error={emailMsg} hint="Used for billing and organization notices; not shown publicly.">
            <Input
              ref={emailRef}
              id={ids.email}
              type="email"
              value={email}
              autoComplete="email"
              required
              invalid={!!emailMsg}
              onChange={(e) => {
                setEmail(e.target.value);
                if (errors.billing_email) setErrors((x) => ({ ...x, billing_email: undefined }));
              }}
              onBlur={() => email && setTouched((t) => ({ ...t, email: true }))}
            />
          </Field>
        </FormStack>

        <Section title="This organization belongs to">
          <RadioCards
            aria-label="This organization belongs to"
            value="personal"
            onChange={() => undefined}
            options={[{ value: 'personal', label: 'My personal account', icon: PersonIcon, description: `${me.login} (you) will be the organization’s first owner.` }]}
          />
        </Section>

        <div className={styles.footer}>
          {formError && (
            <Banner tone="danger" icon={AlertIcon}>
              {formError}
            </Banner>
          )}
          <ButtonRow end>
            <Button onClick={() => history.back()}>Cancel</Button>
            <Button type="submit" variant="primary" loading={busy} disabled={!canSubmit} kbd={formatKeys('mod+enter').join('')}>
              Create organization
            </Button>
          </ButtonRow>
        </div>
      </form>
    </div>
  );
});
