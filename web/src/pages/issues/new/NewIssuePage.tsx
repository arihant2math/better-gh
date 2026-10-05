import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { useResource } from '../../../api/cache';
import { getIssueTemplates, type IssueFormElement, type IssueTemplate, type IssueTemplates } from '../../../api/endpoints';
import { MarkdownEditor } from '../../../components/editor/MarkdownEditor';
import { Link, navigate, useLocation, useParams, useQuery } from '../../../router';
import { formatKeys } from '../../../shortcuts/manager';
import { useShortcuts } from '../../../shortcuts/useShortcuts';
import { store } from '../../../sync';
import type { ID, Repo } from '../../../sync/models';
import { createIssue } from '../../../sync/mutations';
import { assignableUsers, canTriage, labelByName, labelsForRepo, milestonesForRepo, repoByName, userByLogin } from '../../../sync/selectors';
import { Avatar, ColorDot, LabelPill } from '../../../ui/Badge';
import { Button } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { FileIcon, GearIcon, IssueOpenedIcon, LinkExternalIcon, MilestoneIcon } from '../../../ui/icons';
import { Input, Select, Textarea } from '../../../ui/Input';
import { Markdown } from '../../../ui/Markdown';
import { SelectPanel } from '../../../ui/Menu';
import { toast } from '../../../ui/Toast';
import { dropdownOptions, fieldKey, formToMarkdown, initialValues, missingRequired, type FormValues } from './issueForm';
import styles from './NewIssue.module.css';

/** Templates are addressed by basename in URLs (`?template=bug_report.yml`), like GitHub. */
const templateId = (t: IssueTemplate) => t.filename.split('/').pop() ?? t.filename;

/** `/:owner/:repo/issues/new/choose` (chooser) and `/:owner/:repo/issues/new[?template=…]` (form). */
export default observer(function NewIssuePage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const { pathname } = useLocation();
  const repo = repoByName(owner, name);
  const res = useResource<IssueTemplates>(repo ? `issue-templates:${owner}/${name}`.toLowerCase() : null, () => getIssueTemplates(owner, name), { ttlMs: 60_000 });
  if (!repo) return null;
  const choosing = pathname.endsWith('/choose');
  const templates = res.data?.templates ?? [];
  if (choosing) {
    return <Chooser repo={repo} data={res.data} loading={res.loading} failed={!!res.error} />;
  }
  return <NewIssueForm key={pathname + location.search} repo={repo} templates={templates} loading={res.loading && !res.data} />;
});

function Chooser({ repo, data, loading, failed }: { repo: Repo; data: IssueTemplates | undefined; loading: boolean; failed: boolean }) {
  const base = `/${repo.owner}/${repo.name}/issues/new`;
  const templates = data?.templates ?? [];
  const blank = data?.config.blank_issues_enabled ?? true;
  const links = data?.config.contact_links ?? [];
  const listRef = useRef<HTMLDivElement>(null);
  const ready = !!data;
  useEffect(() => {
    if (ready) listRef.current?.querySelector<HTMLElement>('a')?.focus();
  }, [ready]);
  const onKey = (e: KeyboardEvent) => {
    const links = [...(listRef.current?.querySelectorAll<HTMLElement>('a') ?? [])];
    const i = links.indexOf(document.activeElement as HTMLElement);
    const next = e.key === 'ArrowDown' || e.key === 'j' ? i + 1 : e.key === 'ArrowUp' || e.key === 'k' ? i - 1 : null;
    if (next == null) return;
    e.preventDefault();
    links[Math.max(0, Math.min(links.length - 1, next))]?.focus();
  };
  // Without templates there is nothing to choose: go straight to the blank form.
  if (!loading && (failed || (data && templates.length === 0 && links.length === 0))) {
    queueMicrotask(() => navigate(base, { replace: true }));
    return null;
  }
  return (
    <div className={styles.page}>
      <h1 className={styles.heading}>Create new issue in {repo.owner}/{repo.name}</h1>
      <div ref={listRef} className={styles.chooser} role="list" onKeyDown={onKey}>
        {loading && !data
          ? [0, 1].map((i) => (
              <div key={i} className={styles.choice}>
                <Skeleton width="40%" />
                <Skeleton width="70%" style={{ marginTop: 6 }} />
              </div>
            ))
          : templates.map((t) => (
              <Link key={t.filename} to={`${base}?template=${encodeURIComponent(templateId(t))}`} className={styles.choice} role="listitem">
                <span className={styles.choiceIcon}>{t.type === 'form' ? <IssueOpenedIcon size={16} /> : <FileIcon size={16} />}</span>
                <span className={styles.choiceText}>
                  <strong>{t.name}</strong>
                  <span>{t.about}</span>
                </span>
                <span className={styles.choiceGo}>Get started</span>
              </Link>
            ))}
        {links.map((l) => (
          <a key={l.url} href={l.url} target="_blank" rel="noreferrer" className={styles.choice} role="listitem">
            <span className={styles.choiceIcon}>
              <LinkExternalIcon size={16} />
            </span>
            <span className={styles.choiceText}>
              <strong>{l.name}</strong>
              <span>{l.about}</span>
            </span>
            <span className={styles.choiceGo}>Open</span>
          </a>
        ))}
        {blank && (
          <Link to={base} className={styles.choice} role="listitem">
            <span className={styles.choiceIcon}>
              <IssueOpenedIcon size={16} />
            </span>
            <span className={styles.choiceText}>
              <strong>Blank issue</strong>
              <span>Create a new issue from scratch</span>
            </span>
            <span className={styles.choiceGo}>Get started</span>
          </Link>
        )}
      </div>
      {data && data.errors.length > 0 && (
        <div className={styles.templateErrors} role="alert">
          {data.errors.map((e) => (
            <div key={e.filename}>
              <code>{e.filename}</code>: {e.message}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

const NewIssueForm = observer(function NewIssueForm({ repo, templates, loading }: { repo: Repo; templates: IssueTemplate[]; loading: boolean }) {
  const q = useQuery();
  const templateName = q.get('template');
  const template = templateName ? templates.find((t) => templateId(t) === templateName || t.filename === templateName) : undefined;
  if (templateName && loading) {
    return (
      <div className={styles.page}>
        <Skeleton width={260} height={24} />
        <Skeleton width="100%" height={200} style={{ marginTop: 16 }} />
      </div>
    );
  }
  return <Editor repo={repo} template={template} query={q} />;
});

function splitList(v: string | null | undefined): string[] {
  return (v ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
}

const Editor = observer(function Editor({ repo, template, query }: { repo: Repo; template: IssueTemplate | undefined; query: URLSearchParams }) {
  const fullName = `${repo.owner}/${repo.name}`;
  const triage = canTriage(repo.id);
  const [title, setTitle] = useState(() => query.get('title') ?? template?.title ?? '');
  const [body, setBody] = useState(() => query.get('body') ?? (template?.type === 'markdown' ? (template.body ?? '') : ''));
  const form = template?.type === 'form' ? (template.form ?? []) : null;
  const [values, setValues] = useState<FormValues>(() => (form ? initialValues(form) : {}));
  const [showErrors, setShowErrors] = useState(false);
  const [labelIds, setLabelIds] = useState<ID[]>(() =>
    [...(template?.labels ?? []), ...splitList(query.get('labels'))].map((n) => labelByName(repo.id, n)?.id).filter((x): x is ID => x != null),
  );
  const [assigneeIds, setAssigneeIds] = useState<ID[]>(() =>
    [...(template?.assignees ?? []), ...splitList(query.get('assignees'))].map((l) => userByLogin(l)?.id).filter((x): x is ID => x != null),
  );
  const [milestoneId, setMilestoneId] = useState<ID | null>(() => {
    const m = query.get('milestone');
    return m ? (milestonesForRepo(repo.id).find((x) => x.title === m || String(x.number) === m)?.id ?? null) : null;
  });
  const missing = form ? missingRequired(form, values) : [];

  const submit = () => {
    if (!title.trim()) return;
    if (missing.length) {
      setShowErrors(true);
      document.getElementById(`field-${missing[0]}`)?.focus();
      return;
    }
    const text = form ? formToMarkdown(form, values) : body;
    const listPath = `/${repo.owner}/${repo.name}/issues`;
    const { done } = createIssue(repo, {
      title: title.trim(),
      body: text,
      labelIds: triage ? labelIds : [],
      assigneeIds: triage ? assigneeIds : [],
      milestoneId: triage ? milestoneId : null,
    });
    // The row is already in the list (optimistic); open it once it has a number.
    navigate(listPath);
    done.then(
      (res) => {
        const number = (res.data as { number?: number } | undefined)?.number;
        if (!number) return;
        if (window.location.pathname === listPath) navigate(`${listPath}/${number}`);
        else toast({ kind: 'success', title: `Created issue #${number}`, action: { label: 'Open', onClick: () => navigate(`${listPath}/${number}`) } });
      },
      () => undefined,
    );
  };
  useShortcuts('New issue', {
    'mod+enter': { handler: submit, description: 'Create issue', group: 'Issue', allowInInput: true },
  });

  return (
    <div className={styles.page}>
      <div className={styles.topline}>
        <h1 className={styles.heading}>{template ? template.name : 'New issue'}</h1>
        <Link to={`/${repo.owner}/${repo.name}/issues/new/choose`} className={styles.switch}>
          {template ? 'Choose a different template' : 'Use a template'}
        </Link>
      </div>
      {template?.about && <p className={styles.about}>{template.about}</p>}
      <div className={styles.columns}>
        <div className={styles.main}>
          <label className={styles.label} htmlFor="issue-title">
            Add a title <span className={styles.req}>*</span>
          </label>
          <Input id="issue-title" size="lg" autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" />
          {form ? (
            <FormFields form={form} values={values} setValues={setValues} repo={fullName} missing={showErrors ? missing : []} />
          ) : (
            <>
              <label className={styles.label}>Add a description</label>
              <MarkdownEditor value={body} onChange={setBody} repo={fullName} repoId={repo.id} rows={12} placeholder="Type your description here…" ariaLabel="Description" hideActions />
            </>
          )}
          <div className={styles.actions}>
            {showErrors && missing.length > 0 && <span className={styles.error}>Please fill in the required fields.</span>}
            <span className={styles.spacer} />
            <Button variant="ghost" onClick={() => history.back()}>
              Cancel
            </Button>
            <Button variant="primary" disabled={!title.trim()} onClick={submit} kbd={formatKeys('mod+enter')[0]}>
              Create
            </Button>
          </div>
        </div>
        {triage && (
          <MetaPickers
            repo={repo}
            labelIds={labelIds}
            setLabelIds={setLabelIds}
            assigneeIds={assigneeIds}
            setAssigneeIds={setAssigneeIds}
            milestoneId={milestoneId}
            setMilestoneId={setMilestoneId}
          />
        )}
      </div>
    </div>
  );
});

function FormFields({
  form,
  values,
  setValues,
  repo,
  missing,
}: {
  form: IssueFormElement[];
  values: FormValues;
  setValues: (fn: (v: FormValues) => FormValues) => void;
  repo: string;
  missing: string[];
}) {
  const set = (k: string, v: FormValues[string]) => setValues((prev) => ({ ...prev, [k]: v }));
  return (
    <div className={styles.form}>
      {form.map((el, i) => {
        const k = fieldKey(el, i);
        const a = el.attributes ?? {};
        const required = el.validations?.required || (el.type === 'checkboxes' && (a.options ?? []).some((o) => typeof o !== 'string' && o.required));
        const invalid = missing.includes(k);
        const head = (
          <>
            <label className={styles.label} htmlFor={`field-${k}`}>
              {a.label} {required && <span className={styles.req}>*</span>}
            </label>
            {a.description && (
              <div className={styles.desc}>
                <Markdown source={a.description} repo={repo} />
              </div>
            )}
          </>
        );
        let control: ReactNode = null;
        switch (el.type) {
          case 'markdown':
            return (
              <div key={k} className={styles.formMarkdown}>
                <Markdown source={a.value ?? ''} repo={repo} />
              </div>
            );
          case 'input':
            control = <Input id={`field-${k}`} value={String(values[k] ?? '')} placeholder={a.placeholder} invalid={invalid} onChange={(e) => set(k, e.target.value)} />;
            break;
          case 'textarea':
            control = (
              <Textarea
                id={`field-${k}`}
                value={String(values[k] ?? '')}
                placeholder={a.placeholder}
                rows={a.render ? 6 : 5}
                className={a.render ? styles.mono : undefined}
                aria-invalid={invalid}
                onChange={(e) => set(k, e.target.value)}
              />
            );
            break;
          case 'dropdown':
            control = (
              <Select id={`field-${k}`} value={String(values[k] ?? '')} aria-invalid={invalid} onChange={(e) => set(k, e.target.value)}>
                <option value="">Select an option</option>
                {dropdownOptions(el).map((o) => (
                  <option key={o} value={o}>
                    {o}
                  </option>
                ))}
              </Select>
            );
            break;
          case 'checkboxes': {
            const checked = Array.isArray(values[k]) ? (values[k] as boolean[]) : [];
            control = (
              <div className={styles.checks} id={`field-${k}`} tabIndex={-1}>
                {(a.options ?? []).map((o, j) => {
                  const label = typeof o === 'string' ? o : o.label;
                  const req = typeof o !== 'string' && o.required;
                  return (
                    <label key={j} className={styles.check}>
                      <input
                        type="checkbox"
                        checked={!!checked[j]}
                        onChange={(e) => {
                          const next = [...checked];
                          next[j] = e.target.checked;
                          set(k, next);
                        }}
                      />
                      <span>
                        <Markdown source={label} repo={repo} /> {req && <span className={styles.req}>*</span>}
                      </span>
                    </label>
                  );
                })}
              </div>
            );
            break;
          }
        }
        return (
          <div key={k} className={styles.field} data-invalid={invalid || undefined}>
            {head}
            {control}
            {invalid && <div className={styles.error}>This field is required.</div>}
          </div>
        );
      })}
    </div>
  );
}

const MetaPickers = observer(function MetaPickers({
  repo,
  labelIds,
  setLabelIds,
  assigneeIds,
  setAssigneeIds,
  milestoneId,
  setMilestoneId,
}: {
  repo: Repo;
  labelIds: ID[];
  setLabelIds: (v: ID[]) => void;
  assigneeIds: ID[];
  setAssigneeIds: (v: ID[]) => void;
  milestoneId: ID | null;
  setMilestoneId: (v: ID | null) => void;
}) {
  const s = store();
  const [open, setOpen] = useState<null | 'a' | 'l' | 'm'>(null);
  const aRef = useRef<HTMLButtonElement>(null);
  const lRef = useRef<HTMLButtonElement>(null);
  const mRef = useRef<HTMLButtonElement>(null);
  const toggle = (list: ID[], id: ID) => (list.includes(id) ? list.filter((x) => x !== id) : [...list, id]);
  const milestone = s.get('milestone', milestoneId);
  return (
    <aside className={styles.side} aria-label="Issue metadata">
      <button ref={aRef} type="button" className={styles.sideHeader} onClick={() => setOpen('a')}>
        Assignees <GearIcon size={14} />
      </button>
      <div className={styles.sideBody}>
        {assigneeIds.length === 0 ? (
          <span className={styles.subtle}>
            No one —{' '}
            <button type="button" className={styles.linkButton} onClick={() => setAssigneeIds([s.viewerId])}>
              assign yourself
            </button>
          </span>
        ) : (
          assigneeIds.map((id) => (
            <span key={id} className={styles.person}>
              <Avatar user={s.get('user', id)} size={20} /> {s.get('user', id)?.login}
            </span>
          ))
        )}
      </div>
      <SelectPanel
        open={open === 'a'}
        onClose={() => setOpen(null)}
        anchor={aRef}
        placement="bottom-end"
        title="Assign up to 10 people"
        items={assignableUsers(repo).map((u) => ({ id: u.id, text: u.login, description: u.name ?? undefined, leading: <Avatar user={u} size={18} />, selected: assigneeIds.includes(u.id) }))}
        onToggle={(id) => setAssigneeIds(toggle(assigneeIds, Number(id)))}
      />

      <button ref={lRef} type="button" className={styles.sideHeader} onClick={() => setOpen('l')}>
        Labels <GearIcon size={14} />
      </button>
      <div className={styles.sideBody}>
        {labelIds.length === 0 ? (
          <span className={styles.subtle}>None yet</span>
        ) : (
          <div className={styles.labelWrap}>
            {labelIds.map((id) => {
              const l = s.get('label', id);
              return l ? <LabelPill key={id} label={l} /> : null;
            })}
          </div>
        )}
      </div>
      <SelectPanel
        open={open === 'l'}
        onClose={() => setOpen(null)}
        anchor={lRef}
        placement="bottom-end"
        title="Apply labels"
        items={labelsForRepo(repo.id).map((l) => ({ id: l.id, text: l.name, description: l.description ?? undefined, leading: <ColorDot color={l.color} />, selected: labelIds.includes(l.id) }))}
        onToggle={(id) => setLabelIds(toggle(labelIds, Number(id)))}
      />

      <button ref={mRef} type="button" className={styles.sideHeader} onClick={() => setOpen('m')}>
        Milestone <GearIcon size={14} />
      </button>
      <div className={styles.sideBody}>
        {milestone ? (
          <span className={styles.person}>
            <MilestoneIcon size={14} /> {milestone.title}
          </span>
        ) : (
          <span className={styles.subtle}>No milestone</span>
        )}
      </div>
      <SelectPanel
        open={open === 'm'}
        onClose={() => setOpen(null)}
        anchor={mRef}
        placement="bottom-end"
        title="Set milestone"
        multiple={false}
        emptyText="No open milestones"
        items={milestonesForRepo(repo.id)
          .filter((m) => m.state === 'open' && m.id > 0)
          .map((m) => ({ id: m.id, text: m.title, leading: <MilestoneIcon size={14} />, selected: milestoneId === m.id }))}
        onToggle={(id) => setMilestoneId(milestoneId === Number(id) ? null : Number(id))}
      />
    </aside>
  );
});
