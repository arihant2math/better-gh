/** Custom secret scanning patterns of a repository or organization (list, create with a live dry run, delete). */
import { useEffect, useId, useState } from 'react';
import { mutate, useResource } from '../../api/cache';
import {
  createCustomPattern,
  deleteCustomPattern,
  highlightSegments,
  listCustomPatterns,
  ssKeys,
  testPattern,
  type CustomPattern,
  type PatternScope,
  type PatternTestResult,
} from '../../api/secretScanning';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, ItemList, ItemRow, Pill, Section, apiFieldErrors, errorMessage, useDebounced } from '../../components/settings/kit';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CheckCircleIcon, KeyAsteriskIcon, PlusIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Textarea } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import styles from './Security.module.css';

export function CustomPatternsPanel({ scope, description }: { scope: PatternScope; description?: string }) {
  const key = ssKeys.customPatterns(scope);
  const list = useResource(key, () => listCustomPatterns(scope));
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<CustomPattern | null>(null);
  const where = scope.kind === 'org' ? 'every repository of the organization with secret scanning enabled' : 'this repository';
  return (
    <Section
      title="Custom patterns"
      description={description ?? `Regular expressions for your own secret formats, scanned in ${where}.`}
      actions={
        !adding && (
          <Button size="sm" leadingIcon={PlusIcon} onClick={() => setAdding(true)}>
            New pattern
          </Button>
        )
      }
    >
      {adding && (
        <NewPattern
          scope={scope}
          existing={list.data ?? []}
          onCancel={() => setAdding(false)}
          onCreated={(p) => {
            mutate<CustomPattern[]>(key, (prev) => [...(prev ?? []), p]);
            setAdding(false);
          }}
        />
      )}
      {list.error ? (
        <Banner tone="danger">{errorMessage(list.error)}</Banner>
      ) : !list.data ? (
        <Skeleton height={48} />
      ) : (
        <ItemList aria-label="Custom patterns" empty="No custom patterns yet.">
          {list.data.map((p) => (
            <ItemRow
              key={p.id}
              icon={KeyAsteriskIcon}
              title={
                <span style={{ display: 'inline-flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                  {p.name}
                  {p.push_protection && <Pill tone="accent">Push protection</Pill>}
                  {scope.kind === 'repo' && p.scope === 'organization' && <Pill>Organization</Pill>}
                </span>
              }
              meta={
                <>
                  <code className={styles.mono}>{p.pattern}</code>
                  {' · '}added <RelativeTime date={p.created_at} />
                  {p.created_by && ` by @${p.created_by.login}`}
                </>
              }
              actions={
                <Button size="sm" variant="danger" leadingIcon={TrashIcon} aria-label={`Delete pattern ${p.name}`} onClick={() => setDeleting(p)}>
                  Delete
                </Button>
              }
            />
          ))}
        </ItemList>
      )}
      <ConfirmDialog
        open={!!deleting}
        onClose={() => setDeleting(null)}
        title="Delete custom pattern?"
        confirmLabel="Delete pattern"
        onConfirm={async () => {
          if (!deleting) return;
          await deleteCustomPattern(scope, deleting.id);
          mutate<CustomPattern[]>(key, (prev) => (prev ?? []).filter((x) => x.id !== deleting.id));
          toast({ kind: 'success', title: `Pattern “${deleting.name}” deleted` });
        }}
      >
        <p className={styles.muted}>
          New pushes are no longer scanned for <strong>{deleting?.name}</strong>. Existing alerts stay open until you close them.
        </p>
      </ConfirmDialog>
    </Section>
  );
}

/** Live dry run of `pattern` against `text` (debounced). */
function usePatternTest(pattern: string, text: string) {
  const p = useDebounced(pattern, 300);
  const t = useDebounced(text, 300);
  const [state, setState] = useState<{ key: string; result: PatternTestResult | null; error: string | null }>({ key: '', result: null, error: null });
  const key = `${p}\u0000${t}`;
  useEffect(() => {
    if (!p) return;
    let cancelled = false;
    testPattern(p, t).then(
      (result) => !cancelled && setState({ key, result, error: null }),
      (e: unknown) => !cancelled && setState({ key, result: null, error: errorMessage(e) }),
    );
    return () => {
      cancelled = true;
    };
  }, [p, t, key]);
  const settled = state.key === key && p === pattern && t === text;
  return { result: p && settled ? state.result : null, error: p && settled ? state.error : null, pending: !!pattern && !settled };
}

function NewPattern({ scope, existing, onCancel, onCreated }: { scope: PatternScope; existing: CustomPattern[]; onCancel: () => void; onCreated: (p: CustomPattern) => void }) {
  const ids = { name: useId(), pattern: useId(), test: useId() };
  const [name, setName] = useState('');
  const [pattern, setPattern] = useState('');
  const [test, setTest] = useState('');
  const [push, setPush] = useState(false);
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [server, setServer] = useState<{ message: string | null; fields: Record<string, string | undefined> }>({ message: null, fields: {} });
  const dry = usePatternTest(pattern, test);
  const nameErr = !name.trim() ? 'Enter a name.' : existing.some((p) => p.name.toLowerCase() === name.trim().toLowerCase()) ? 'A pattern with this name already exists.' : null;
  const patternErr = !pattern ? 'Enter a regular expression.' : dry.result && !dry.result.valid ? (dry.result.error ?? 'Invalid regular expression.') : null;

  const submit = async () => {
    setTouched(true);
    if (nameErr || patternErr || busy) return;
    setBusy(true);
    setServer({ message: null, fields: {} });
    try {
      const p = await createCustomPattern(scope, { name: name.trim(), pattern, test_string: test || null, push_protection: push });
      toast({ kind: 'success', title: `Pattern “${p.name}” created` });
      onCreated(p);
    } catch (e) {
      setServer(apiFieldErrors(e));
    } finally {
      setBusy(false);
    }
  };

  const matches = dry.result?.valid ? dry.result.matches : [];
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
      style={{ marginBottom: 'var(--sp-4)' }}
      aria-label="New custom pattern"
    >
      <FormStack wide>
        <Field label="Pattern name" htmlFor={ids.name} error={(touched && nameErr) || server.fields.name || null}>
          <Input id={ids.name} value={name} autoFocus autoComplete="off" placeholder="Acme API token" invalid={(touched && !!nameErr) || !!server.fields.name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field
          label="Secret format"
          htmlFor={ids.pattern}
          error={(touched || !!pattern) && patternErr ? patternErr : server.fields.pattern || null}
          hint="A regular expression matching the secret, e.g. acme_[a-z0-9]{32}."
        >
          <Input
            id={ids.pattern}
            value={pattern}
            spellCheck={false}
            autoComplete="off"
            className={styles.mono}
            placeholder="acme_[a-z0-9]{32}"
            invalid={((touched || !!pattern) && !!patternErr) || !!server.fields.pattern}
            onChange={(e) => setPattern(e.target.value)}
          />
        </Field>
        <Field label="Test string" htmlFor={ids.test} hint="Paste sample text; matches are highlighted below.">
          <Textarea id={ids.test} rows={3} value={test} spellCheck={false} className={styles.mono} onChange={(e) => setTest(e.target.value)} />
        </Field>
        <div className={styles.section}>
          <div className={styles.testStatus} role="status" aria-live="polite">
            {!pattern ? (
              <span className={styles.muted}>Enter a pattern to test it.</span>
            ) : dry.pending ? (
              <>
                <Spinner size={14} /> Testing…
              </>
            ) : dry.error ? (
              <>
                <AlertIcon size={14} /> {dry.error}
              </>
            ) : dry.result && !dry.result.valid ? (
              <>
                <AlertIcon size={14} /> Invalid pattern
              </>
            ) : (
              <>
                <CheckCircleIcon size={14} className={styles.on} /> {matches.length} {matches.length === 1 ? 'match' : 'matches'} in the test string
              </>
            )}
          </div>
          {test && (
            <div className={styles.testOutput} data-testid="pattern-test-output">
              {highlightSegments(test, matches).map((s, i) =>
                s.match ? (
                  <mark key={i} className={styles.match}>
                    {s.text}
                  </mark>
                ) : (
                  <span key={i}>{s.text}</span>
                ),
              )}
            </div>
          )}
        </div>
        <Checkbox
          label="Include in push protection"
          description="Block pushes that contain a match (when push protection is on)."
          checked={push}
          onChange={setPush}
        />
        {server.message && !server.fields.name && !server.fields.pattern && <Banner tone="danger">{server.message}</Banner>}
        <ButtonRow>
          <Button type="submit" variant="primary" loading={busy}>
            Create pattern
          </Button>
          <Button onClick={onCancel}>Cancel</Button>
        </ButtonRow>
      </FormStack>
    </form>
  );
}
