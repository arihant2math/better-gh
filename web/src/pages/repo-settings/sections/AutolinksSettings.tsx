import { useId, useState } from 'react';
import { createAutolink, deleteAutolink, listAutolinks, type Autolink } from '../../../api/repoSettings';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, Section, apiFieldErrors } from '../../../components/settings/kit';
import { Button } from '../../../ui/Button';
import { LinkIcon, PlusIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import styles from '../RepoSettings.module.css';
import { ListSkeleton, LoadError, repoKey, useLocalResource, type SectionProps } from '../shared';
import { autolinkPrefixError, autolinkTemplateError } from '../validation';

export default function AutolinksSettings({ repo }: SectionProps) {
  const links = useLocalResource(repoKey(repo, 'autolinks'), () => listAutolinks(repo.owner, repo.name));
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<Autolink | null>(null);
  return (
    <>
      <PageHeader
        title="Autolink references"
        description="Autolinks turn references like JIRA-123 in issues, pull requests, commit messages and release descriptions into links to external systems."
        actions={
          !adding && (
            <Button variant="primary" size="sm" leadingIcon={PlusIcon} onClick={() => setAdding(true)}>
              Add autolink reference
            </Button>
          )
        }
      />
      {adding && (
        <NewAutolink
          repo={repo}
          existing={links.data ?? []}
          onCancel={() => setAdding(false)}
          onCreated={(l) => {
            links.update((list) => [...list, l]);
            setAdding(false);
          }}
        />
      )}
      {links.error ? <LoadError error={links.error} /> : null}
      {!links.data ? (
        links.error ? null : <ListSkeleton rows={2} />
      ) : (
        <ItemList aria-label="Autolink references" empty="No autolink references yet.">
          {links.data.map((l) => (
            <ItemRow
              key={l.id}
              icon={LinkIcon}
              title={
                <span className={styles.row}>
                  <span className={styles.mono}>{l.key_prefix}&lt;num&gt;</span>
                  <Pill>{l.is_alphanumeric ? 'Alphanumeric' : 'Numeric'}</Pill>
                </span>
              }
              meta={<span className={styles.mono}>{l.url_template}</span>}
              actions={
                <Button size="sm" variant="danger" leadingIcon={TrashIcon} aria-label={`Delete autolink ${l.key_prefix}`} onClick={() => setDeleting(l)}>
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
        title="Delete autolink reference?"
        confirmLabel="Delete reference"
        onConfirm={async () => {
          if (!deleting) return;
          await deleteAutolink(repo.owner, repo.name, deleting.id);
          links.update((list) => list.filter((x) => x.id !== deleting.id));
          toast({ kind: 'success', title: `Autolink ${deleting.key_prefix} deleted` });
        }}
      >
        <p className={styles.muted}>
          References starting with <code>{deleting?.key_prefix}</code> will no longer be linked.
        </p>
      </ConfirmDialog>
    </>
  );
}

function NewAutolink({
  repo,
  existing,
  onCancel,
  onCreated,
}: {
  repo: SectionProps['repo'];
  existing: Autolink[];
  onCancel: () => void;
  onCreated: (l: Autolink) => void;
}) {
  const ids = { prefix: useId(), url: useId() };
  const [prefix, setPrefix] = useState('');
  const [url, setUrl] = useState('');
  const [alnum, setAlnum] = useState(true);
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [server, setServer] = useState<{ message: string | null; fields: Record<string, string | undefined> }>({ message: null, fields: {} });
  const dup = existing.some((l) => l.key_prefix.toLowerCase() === prefix.toLowerCase()) ? 'An autolink with this prefix already exists.' : null;
  const prefixErr = autolinkPrefixError(prefix) ?? dup;
  const urlErr = autolinkTemplateError(url);
  const sample = !urlErr && !prefixErr ? url.trim().replace('<num>', alnum ? 'ABC123' : '123') : null;

  const submit = async () => {
    setTouched(true);
    if (prefixErr || urlErr || busy) return;
    setBusy(true);
    setServer({ message: null, fields: {} });
    try {
      const l = await createAutolink(repo.owner, repo.name, { key_prefix: prefix, url_template: url.trim(), is_alphanumeric: alnum });
      toast({ kind: 'success', title: `Autolink ${l.key_prefix} added` });
      onCreated(l);
    } catch (e) {
      setServer(apiFieldErrors(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title="Add autolink reference">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <FormStack>
          <Field
            label="Reference prefix"
            htmlFor={ids.prefix}
            error={(touched && prefixErr) || server.fields.key_prefix || null}
            hint="This prefix appended by a number will generate a link any time it is found in an issue, pull request, or commit."
          >
            <Input
              id={ids.prefix}
              value={prefix}
              autoFocus
              placeholder="TICKET-"
              spellCheck={false}
              autoComplete="off"
              invalid={(touched && !!prefixErr) || !!server.fields.key_prefix}
              onChange={(e) => setPrefix(e.target.value)}
              onKeyDown={(e) => e.key === 'Escape' && onCancel()}
            />
          </Field>
          <Field
            label="Target URL"
            htmlFor={ids.url}
            error={(touched && urlErr) || server.fields.url_template || null}
            hint={sample ? <>Example: {prefix}{alnum ? 'ABC123' : '123'} → {sample}</> : 'The URL must contain <num> for the reference number.'}
          >
            <Input
              id={ids.url}
              value={url}
              placeholder="https://example.com/TICKET?query=<num>"
              spellCheck={false}
              autoComplete="off"
              invalid={(touched && !!urlErr) || !!server.fields.url_template}
              onChange={(e) => setUrl(e.target.value)}
              onKeyDown={(e) => e.key === 'Escape' && onCancel()}
            />
          </Field>
          <Checkbox
            label="Alphanumeric"
            description="<num> matches letters A-Z (case insensitive), numbers 0-9 and -. Uncheck to match numbers only."
            checked={alnum}
            onChange={setAlnum}
          />
          {server.message && !server.fields.key_prefix && !server.fields.url_template && <Banner tone="danger">{server.message}</Banner>}
          <ButtonRow>
            <Button type="submit" variant="primary" loading={busy}>
              Add autolink reference
            </Button>
            <Button onClick={onCancel}>Cancel</Button>
          </ButtonRow>
        </FormStack>
      </form>
    </Section>
  );
}
