import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { navigate } from '../../router';
import { store } from '../../sync';
import type { ID, ProjectField, ProjectFieldOption, ProjectFieldType, ProjectIteration, ProjectIterationConfig, ProjectWorkflowKind } from '../../sync/models';
import {
  createField,
  deleteField,
  deleteProject,
  fieldsForProject,
  setRepoLinked,
  setWorkflow,
  tempOptionId,
  updateField,
  updateProject,
  workflowsForProject,
} from '../../sync/projects';
import { reposForOwner } from '../../sync/selectors';
import { ColorDot } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { ArrowDownIcon, ArrowUpIcon, PlusIcon, TrashIcon, XIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import type { ProjectCtx } from './data';
import { FIELD_TYPE_LABEL, OPTION_COLOR_NAMES, addDays, isBuiltin, isSelectLike, iterationRange, optionHex, todayYmd } from './fields';
import styles from './Projects.module.css';

export type SettingsTab = 'general' | 'fields' | 'workflows';

export const SettingsDialog = observer(function SettingsDialog({
  ctx,
  tab,
  fieldId,
  onTab,
  onClose,
}: {
  ctx: ProjectCtx;
  tab: SettingsTab;
  fieldId?: string | null;
  onTab: (t: SettingsTab, field?: string | null) => void;
  onClose: () => void;
}) {
  return (
    <Dialog open onClose={onClose} title="Project settings" className={styles.settings}>
      <div className={styles.settingsTabs}>
        <Tabs
          value={tab}
          onChange={(t) => onTab(t as SettingsTab)}
          items={[
            { id: 'general', label: 'General' },
            { id: 'fields', label: 'Fields' },
            { id: 'workflows', label: 'Workflows' },
          ]}
        />
      </div>
      {tab === 'general' ? (
        <General ctx={ctx} />
      ) : tab === 'fields' ? (
        <Fields ctx={ctx} fieldId={fieldId ?? null} onSelect={(f) => onTab('fields', f)} />
      ) : (
        <Workflows ctx={ctx} />
      )}
    </Dialog>
  );
});

// ------------------------------------------------------------------ general

const General = observer(function General({ ctx }: { ctx: ProjectCtx }) {
  const p = ctx.project;
  const [title, setTitle] = useState(p.title);
  const [desc, setDesc] = useState(p.shortDescription ?? '');
  const [readme, setReadme] = useState(p.readme ?? '');
  const [preview, setPreview] = useState(false);
  const [confirm, setConfirm] = useState('');
  const dirty = title !== p.title || desc !== (p.shortDescription ?? '') || readme !== (p.readme ?? '');
  const ownerRepos = reposForOwner(p.ownerId);
  const ro = !ctx.canWrite;
  return (
    <div className={styles.settingsBody}>
      <Field label="Project name" htmlFor="ps-title">
        <Input id="ps-title" value={title} disabled={ro} onChange={(e) => setTitle(e.target.value)} />
      </Field>
      <Field label="Short description" htmlFor="ps-desc">
        <Input id="ps-desc" value={desc} disabled={ro} onChange={(e) => setDesc(e.target.value)} />
      </Field>
      <Field label="README" hint="Markdown. Shown on the project and in the project list.">
        <div className={styles.readmeTabs}>
          <Tabs
            size="sm"
            value={preview ? 'preview' : 'write'}
            onChange={(t) => setPreview(t === 'preview')}
            items={[
              { id: 'write', label: 'Write' },
              { id: 'preview', label: 'Preview' },
            ]}
          />
        </div>
        {preview ? (
          <Markdown source={readme} className={styles.readmePreview} />
        ) : (
          <Textarea rows={8} value={readme} disabled={ro} onChange={(e) => setReadme(e.target.value)} aria-label="README" />
        )}
      </Field>
      {ctx.canWrite && (
        <div className={styles.rowEnd}>
          <Button
            variant="primary"
            disabled={!dirty || !title.trim()}
            onClick={() => {
              updateProject(p, { title: title.trim(), shortDescription: desc.trim() || null, readme: readme || null });
              toast({ kind: 'success', title: 'Project saved' });
            }}
          >
            Save changes
          </Button>
        </div>
      )}

      {ctx.canWrite && ownerRepos.length > 0 && (
        <section className={styles.settingsSection}>
          <h3>Linked repositories</h3>
          <p className={styles.muted}>Linked repositories list this project in their Projects tab, and their issues can be added with #number.</p>
          <div className={styles.repoChecks}>
            {ownerRepos.map((r) => (
              <label key={r.id} className={styles.check}>
                <input type="checkbox" checked={p.linkedRepoIds.includes(r.id)} onChange={(e) => setRepoLinked(p, r.id, e.target.checked)} />
                {r.owner}/{r.name}
              </label>
            ))}
          </div>
        </section>
      )}

      {ctx.canAdmin && (
        <section className={cx(styles.settingsSection, styles.danger)}>
          <h3>Danger zone</h3>
          <div className={styles.dangerRow}>
            <div>
              <strong>Visibility: {p.public ? 'Public' : 'Private'}</strong>
              <p className={styles.muted}>{p.public ? 'Anyone can see this project.' : 'Only members of the owner can see this project.'}</p>
            </div>
            <Button onClick={() => updateProject(p, { public: !p.public })}>Make {p.public ? 'private' : 'public'}</Button>
          </div>
          <div className={styles.dangerRow}>
            <div>
              <strong>{p.closed ? 'Reopen project' : 'Close project'}</strong>
              <p className={styles.muted}>
                {p.closed ? 'Reopened projects appear in the open list again.' : 'Closed projects are read-only and hidden from the default list.'}
              </p>
            </div>
            <Button onClick={() => updateProject(p, { closed: !p.closed })}>{p.closed ? 'Reopen' : 'Close'} project</Button>
          </div>
          <div className={styles.dangerRow}>
            <div>
              <strong>Delete project</strong>
              <p className={styles.muted}>Type the project name to confirm. Items keep existing in their repositories.</p>
              <Input size="sm" value={confirm} onChange={(e) => setConfirm(e.target.value)} placeholder={p.title} aria-label="Confirm project name" />
            </div>
            <Button
              variant="danger"
              disabled={confirm !== p.title}
              onClick={() => {
                deleteProject(p);
                navigate(`/${ctx.ownerKind}/${ctx.owner}/projects`);
              }}
            >
              Delete
            </Button>
          </div>
        </section>
      )}
    </div>
  );
});

// ------------------------------------------------------------------ fields

const NEW_TYPES: ProjectFieldType[] = ['text', 'number', 'date', 'single_select', 'iteration'];

const Fields = observer(function Fields({ ctx, fieldId, onSelect }: { ctx: ProjectCtx; fieldId: string | null; onSelect: (id: string | null) => void }) {
  const fields = fieldsForProject(ctx.project.id);
  const selected = fieldId === 'new' ? null : fields.find((f) => String(f.id) === fieldId);
  return (
    <div className={styles.fieldsLayout}>
      <nav className={styles.fieldNav} aria-label="Fields">
        {fields.map((f) => (
          <button
            key={f.id}
            type="button"
            className={styles.fieldNavItem}
            aria-current={selected?.id === f.id || undefined}
            onClick={() => onSelect(String(f.id))}
          >
            <span>{f.name}</span>
            <span className={styles.muted}>{FIELD_TYPE_LABEL[f.dataType]}</span>
          </button>
        ))}
        {ctx.canWrite && (
          <Button size="sm" variant="ghost" leadingIcon={PlusIcon} onClick={() => onSelect('new')}>
            New field
          </Button>
        )}
      </nav>
      <div className={styles.fieldEditor}>
        {fieldId === 'new' ? (
          <NewField ctx={ctx} onCreated={() => onSelect(null)} />
        ) : selected ? (
          <FieldEditor key={selected.id} ctx={ctx} field={selected} onDeleted={() => onSelect(null)} />
        ) : (
          <p className={styles.muted}>Select a field to edit it, or create a new one.</p>
        )}
      </div>
    </div>
  );
});

function defaultIterations(start = todayYmd(), duration = 14, count = 3): ProjectIterationConfig {
  return {
    startDate: start,
    duration,
    iterations: Array.from({ length: count }, (_, i) => ({
      id: tempOptionId(),
      title: `Iteration ${i + 1}`,
      startDate: addDays(start, i * duration),
      duration,
    })),
  };
}

const NewField = observer(function NewField({ ctx, onCreated }: { ctx: ProjectCtx; onCreated: () => void }) {
  const [name, setName] = useState('');
  const [type, setType] = useState<ProjectFieldType>('text');
  const [options, setOptions] = useState<ProjectFieldOption[]>([{ id: tempOptionId(), name: '', color: 'GRAY', description: '' }]);
  const [start, setStart] = useState(todayYmd());
  const [duration, setDuration] = useState(14);
  const taken = fieldsForProject(ctx.project.id).some((f) => f.name.toLowerCase() === name.trim().toLowerCase());
  return (
    <form
      className={styles.settingsBody}
      onSubmit={(e) => {
        e.preventDefault();
        if (!name.trim() || taken) return;
        createField(ctx.project, {
          name: name.trim(),
          dataType: type,
          options: type === 'single_select' ? options.filter((o) => o.name.trim()) : null,
          iterations: type === 'iteration' ? defaultIterations(start, duration) : null,
        });
        onCreated();
      }}
    >
      <Field label="Field name" htmlFor="nf-name" error={taken ? 'A field with this name already exists' : null}>
        <Input id="nf-name" autoFocus value={name} onChange={(e) => setName(e.target.value)} invalid={taken} />
      </Field>
      <Field label="Field type" htmlFor="nf-type">
        <Select id="nf-type" value={type} onChange={(e) => setType(e.target.value as ProjectFieldType)}>
          {NEW_TYPES.map((t) => (
            <option key={t} value={t}>
              {FIELD_TYPE_LABEL[t]}
            </option>
          ))}
        </Select>
      </Field>
      {type === 'single_select' && <OptionsEditor options={options} onChange={setOptions} />}
      {type === 'iteration' && (
        <div className={styles.inlineFields}>
          <Field label="Starts on" htmlFor="nf-start">
            <Input id="nf-start" type="date" value={start} onChange={(e) => setStart(e.target.value)} />
          </Field>
          <Field label="Duration (days)" htmlFor="nf-dur">
            <Input id="nf-dur" type="number" min={1} max={84} value={duration} onChange={(e) => setDuration(Math.max(1, Number(e.target.value) || 14))} />
          </Field>
        </div>
      )}
      <div className={styles.rowEnd}>
        <Button type="submit" variant="primary" disabled={!name.trim() || taken}>
          Save field
        </Button>
      </div>
    </form>
  );
});

function OptionsEditor({ options, onChange }: { options: ProjectFieldOption[]; onChange: (o: ProjectFieldOption[]) => void }) {
  const set = (i: number, patch: Partial<ProjectFieldOption>) => onChange(options.map((o, k) => (k === i ? { ...o, ...patch } : o)));
  const move = (i: number, d: number) => {
    const j = i + d;
    if (j < 0 || j >= options.length) return;
    const next = [...options];
    [next[i], next[j]] = [next[j]!, next[i]!];
    onChange(next);
  };
  return (
    <div className={styles.options}>
      <div className={styles.fieldLabel}>Options</div>
      {options.map((o, i) => (
        <div key={o.id} className={styles.optionRow}>
          <ColorDot color={optionHex(o.color)} />
          <Input
            size="sm"
            className={styles.optInput}
            value={o.name}
            placeholder="Option name"
            onChange={(e) => set(i, { name: e.target.value })}
            aria-label="Option name"
          />
          <Select value={String(o.color).toUpperCase()} onChange={(e) => set(i, { color: e.target.value })} aria-label="Color" className={styles.colorSelect}>
            {OPTION_COLOR_NAMES.map((c) => (
              <option key={c} value={c}>
                {c.charAt(0) + c.slice(1).toLowerCase()}
              </option>
            ))}
          </Select>
          <Input
            size="sm"
            className={styles.optInput}
            value={o.description}
            placeholder="Description"
            onChange={(e) => set(i, { description: e.target.value })}
            aria-label="Option description"
          />
          <IconButton icon={ArrowUpIcon} label="Move up" size="sm" disabled={i === 0} onClick={() => move(i, -1)} />
          <IconButton icon={ArrowDownIcon} label="Move down" size="sm" disabled={i === options.length - 1} onClick={() => move(i, 1)} />
          <IconButton icon={XIcon} label="Remove option" size="sm" onClick={() => onChange(options.filter((_, k) => k !== i))} />
        </div>
      ))}
      <Button
        size="sm"
        variant="ghost"
        leadingIcon={PlusIcon}
        onClick={() =>
          onChange([...options, { id: tempOptionId(), name: '', color: OPTION_COLOR_NAMES[options.length % OPTION_COLOR_NAMES.length]!, description: '' }])
        }
      >
        Add option
      </Button>
    </div>
  );
}

function IterationsEditor({ config, onChange }: { config: ProjectIterationConfig; onChange: (c: ProjectIterationConfig) => void }) {
  const list = [...config.iterations].sort((a, b) => (a.startDate < b.startDate ? -1 : 1));
  const last = list.at(-1);
  const add = (gap: number) => {
    const start = last ? addDays(last.startDate, last.duration + gap) : config.startDate;
    const it: ProjectIteration = { id: tempOptionId(), title: `Iteration ${list.length + 1}`, startDate: start, duration: config.duration };
    onChange({ ...config, iterations: [...list, it] });
  };
  const set = (id: string, patch: Partial<ProjectIteration>) => onChange({ ...config, iterations: list.map((x) => (x.id === id ? { ...x, ...patch } : x)) });
  return (
    <div className={styles.options}>
      <div className={styles.inlineFields}>
        <Field label="Starts on" htmlFor="it-start">
          <Input id="it-start" type="date" value={config.startDate} onChange={(e) => onChange({ ...config, startDate: e.target.value })} />
        </Field>
        <Field label="Default duration (days)" htmlFor="it-dur">
          <Input
            id="it-dur"
            type="number"
            min={1}
            max={84}
            value={config.duration}
            onChange={(e) => onChange({ ...config, duration: Math.max(1, Number(e.target.value) || 14) })}
          />
        </Field>
      </div>
      <div className={styles.fieldLabel}>Iterations</div>
      {list.map((it, i) => {
        const prev = list[i - 1];
        const gap = prev ? (Date.parse(it.startDate) - Date.parse(addDays(prev.startDate, prev.duration))) / 86_400_000 : 0;
        return (
          <div key={it.id}>
            {gap > 0 && <div className={styles.iterBreak}>Break · {gap} days</div>}
            <div className={styles.optionRow}>
              <Input
                size="sm"
                className={styles.optInput}
                value={it.title}
                onChange={(e) => set(it.id, { title: e.target.value })}
                aria-label="Iteration title"
              />
              <Input
                size="sm"
                className={styles.optInput}
                type="date"
                value={it.startDate}
                onChange={(e) => set(it.id, { startDate: e.target.value })}
                aria-label="Start date"
              />
              <Input
                size="sm"
                type="number"
                min={1}
                value={it.duration}
                onChange={(e) => set(it.id, { duration: Math.max(1, Number(e.target.value) || 1) })}
                aria-label="Duration"
                className={styles.durInput}
              />
              <span className={styles.muted}>{iterationRange(it)}</span>
              <IconButton
                icon={XIcon}
                label="Remove iteration"
                size="sm"
                onClick={() => onChange({ ...config, iterations: list.filter((x) => x.id !== it.id) })}
              />
            </div>
          </div>
        );
      })}
      <div className={styles.row}>
        <Button size="sm" variant="ghost" leadingIcon={PlusIcon} onClick={() => add(0)}>
          Add iteration
        </Button>
        <Button size="sm" variant="ghost" onClick={() => add(7)}>
          Add break (1 week) + iteration
        </Button>
      </div>
    </div>
  );
}

const FieldEditor = observer(function FieldEditor({ ctx, field, onDeleted }: { ctx: ProjectCtx; field: ProjectField; onDeleted: () => void }) {
  const [name, setName] = useState(field.name);
  const [options, setOptions] = useState<ProjectFieldOption[]>(field.options ?? []);
  const [iterations, setIterations] = useState<ProjectIterationConfig>(field.iterations ?? defaultIterations());
  const builtin = isBuiltin(field);
  const ro = !ctx.canWrite;
  const optionsDirty = isSelectLike(field) && JSON.stringify(options) !== JSON.stringify(field.options ?? []);
  const itersDirty = field.dataType === 'iteration' && JSON.stringify(iterations) !== JSON.stringify(field.iterations);
  const dirty = name.trim() !== field.name || optionsDirty || itersDirty;
  return (
    <div className={styles.settingsBody}>
      <Field label="Field name" htmlFor="fe-name" hint={`${FIELD_TYPE_LABEL[field.dataType]}${builtin ? ' · built-in' : ''}`}>
        <Input id="fe-name" value={name} disabled={ro || (builtin && field.dataType !== 'status')} onChange={(e) => setName(e.target.value)} />
      </Field>
      {isSelectLike(field) && !ro && <OptionsEditor options={options} onChange={setOptions} />}
      {field.dataType === 'iteration' && !ro && <IterationsEditor config={iterations} onChange={setIterations} />}
      {!ro && (
        <div className={styles.rowBetween}>
          {!builtin ? (
            <Button
              variant="danger"
              leadingIcon={TrashIcon}
              onClick={() => {
                deleteField(ctx.project, field);
                onDeleted();
              }}
            >
              Delete field
            </Button>
          ) : (
            <span />
          )}
          <Button
            variant="primary"
            disabled={!dirty || !name.trim()}
            onClick={() => {
              const patch: Parameters<typeof updateField>[2] = {};
              if (name.trim() !== field.name) patch.name = name.trim();
              if (optionsDirty) patch.options = options.filter((o) => o.name.trim());
              if (itersDirty) patch.iterations = iterations;
              updateField(ctx.project, field, patch);
              toast({ kind: 'success', title: `Saved ${name.trim()}` });
            }}
          >
            Save
          </Button>
        </div>
      )}
    </div>
  );
});

// ------------------------------------------------------------------ workflows

const WORKFLOWS: { kind: ProjectWorkflowKind; title: string; text: string; status?: boolean }[] = [
  { kind: 'item_added', title: 'Item added to project', text: 'Set the status of items when they are added.', status: true },
  { kind: 'item_reopened', title: 'Item reopened', text: 'Set the status when an issue or pull request is reopened.', status: true },
  { kind: 'item_closed', title: 'Item closed', text: 'Set the status when an issue or pull request is closed.', status: true },
  { kind: 'pr_merged', title: 'Pull request merged', text: 'Set the status when a pull request is merged.', status: true },
  { kind: 'auto_add', title: 'Auto-add to project', text: 'Add new issues and pull requests from repositories that match a filter.' },
  { kind: 'auto_archive', title: 'Auto-archive items', text: 'Archive items when they are closed or merged.' },
];

const Workflows = observer(function Workflows({ ctx }: { ctx: ProjectCtx }) {
  const list = workflowsForProject(ctx.project.id);
  const status = fieldsForProject(ctx.project.id).find((f) => f.dataType === 'status');
  const repos = reposForOwner(ctx.project.ownerId);
  const ro = !ctx.canWrite;
  return (
    <div className={styles.settingsBody}>
      {WORKFLOWS.map((w) => {
        const row = list.find((x) => x.kind === w.kind);
        const enabled = !!row?.enabled;
        const config = row?.config ?? {};
        const put = (en: boolean, cfg = config) => setWorkflow(ctx.project, w.kind, en, cfg);
        return (
          <section key={w.kind} className={styles.workflow}>
            <div className={styles.workflowHead}>
              <div>
                <strong>{w.title}</strong>
                <p className={styles.muted}>{w.text}</p>
              </div>
              <label className={styles.switch}>
                <input
                  type="checkbox"
                  role="switch"
                  checked={enabled}
                  disabled={ro}
                  onChange={(e) => put(e.target.checked)}
                  aria-label={`${w.title} enabled`}
                />
                <span>{enabled ? 'On' : 'Off'}</span>
              </label>
            </div>
            {w.status && status && (
              <div className={styles.workflowCfg}>
                <span>Set Status to</span>
                <Select
                  value={config.statusOptionId ?? ''}
                  disabled={ro}
                  onChange={(e) => put(enabled, { ...config, statusOptionId: e.target.value || undefined })}
                  aria-label="Status"
                >
                  <option value="">—</option>
                  {(status.options ?? []).map((o) => (
                    <option key={o.id} value={o.id}>
                      {o.name}
                    </option>
                  ))}
                </Select>
              </div>
            )}
            {w.kind === 'auto_add' && (
              <div className={styles.workflowCfg}>
                <span>From</span>
                <div className={styles.repoChecks}>
                  {repos.map((r) => (
                    <label key={r.id} className={styles.check}>
                      <input
                        type="checkbox"
                        disabled={ro}
                        checked={(config.repoIds ?? []).includes(r.id)}
                        onChange={(e) => {
                          const ids: ID[] = e.target.checked ? [...(config.repoIds ?? []), r.id] : (config.repoIds ?? []).filter((x) => x !== r.id);
                          put(enabled, { ...config, repoIds: ids });
                        }}
                      />
                      {r.name}
                    </label>
                  ))}
                  {repos.length === 0 && <span className={styles.muted}>No repositories in your local store for this owner.</span>}
                </div>
                <span>matching</span>
                <FilterInput value={config.filter ?? 'is:issue,pr is:open'} disabled={ro} onSave={(f) => put(enabled, { ...config, filter: f })} />
              </div>
            )}
          </section>
        );
      })}
      {store().get('project', ctx.project.id) ? null : <p className={styles.muted}>Workflows can be edited once the project is saved.</p>}
    </div>
  );
});

function FilterInput({ value, disabled, onSave }: { value: string; disabled: boolean; onSave: (v: string) => void }) {
  const [v, setV] = useState(value);
  return (
    <Input
      size="sm"
      value={v}
      disabled={disabled}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => v !== value && onSave(v)}
      onKeyDown={(e) => e.key === 'Enter' && v !== value && onSave(v)}
      aria-label="Auto-add filter"
      className={styles.wfFilter}
    />
  );
}
