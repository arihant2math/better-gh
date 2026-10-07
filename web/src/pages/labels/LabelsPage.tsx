import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useCommands } from '../../app/commands';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import { ColorPicker } from '../../components/labels/ColorPicker';
import { randomLabelColor } from '../../components/labels/colors';
import { Link, setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { useComputed } from '../../sync/hooks';
import type { Label, Repo } from '../../sync/models';
import { createLabel, deleteLabel, updateLabel } from '../../sync/mutations';
import { canPush, labelByName, labelsForRepo, labelUsage, repoByName } from '../../sync/selectors';
import { LabelPill } from '../../ui/Badge';
import { Button, cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { IssueOpenedIcon, PlusIcon, SearchIcon, TagIcon } from '../../ui/icons';
import { Input, Select } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import styles from './Labels.module.css';

type Sort = 'name' | 'name-desc' | 'most' | 'fewest';
const SORTS: { key: Sort; label: string }[] = [
  { key: 'name', label: 'Alphabetically' },
  { key: 'name-desc', label: 'Reverse alphabetically' },
  { key: 'most', label: 'Most issues' },
  { key: 'fewest', label: 'Fewest issues' },
];

const q = (s: string) => (/[\s"]/.test(s) ? `"${s.replace(/"/g, '')}"` : s);

export default observer(function LabelsPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(owner, name);
  if (!repo) return null;
  return <Labels repo={repo} />;
});

const Labels = observer(function Labels({ repo }: { repo: Repo }) {
  const query = useQuery();
  const filter = query.get('q') ?? '';
  const sort = (query.get('sort') as Sort | null) ?? 'name';
  const writable = canPush(repo.id);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<number | null>(null);
  const [active, setActive] = useState(0);
  const [confirm, setConfirm] = useState<Label | null>(null);
  const usage = useComputed(() => labelUsage(repo.id), [repo.id]);
  const labels = useComputed(() => {
    const f = filter.toLowerCase();
    const list = labelsForRepo(repo.id).filter((l) => !f || l.name.toLowerCase().includes(f) || (l.description ?? '').toLowerCase().includes(f));
    const n = (l: Label) => usage.get(l.id) ?? 0;
    if (sort === 'name-desc') list.reverse();
    else if (sort === 'most') list.sort((a, b) => n(b) - n(a) || a.name.localeCompare(b.name));
    else if (sort === 'fewest') list.sort((a, b) => n(a) - n(b) || a.name.localeCompare(b.name));
    return list;
  }, [repo.id, filter, sort, usage]);
  const cursor = Math.min(active, Math.max(0, labels.length - 1));
  const base = `/${repo.owner}/${repo.name}`;

  useShortcuts('Labels', {
    n: { handler: () => (writable ? setCreating(true) : false), description: 'New label', group: 'Labels' },
    j: { handler: () => setActive(Math.min(labels.length - 1, cursor + 1)), description: 'Next label', group: 'Labels' },
    k: { handler: () => setActive(Math.max(0, cursor - 1)), description: 'Previous label', group: 'Labels' },
    e: { handler: () => (writable && labels[cursor] ? setEditing(labels[cursor].id) : false), description: 'Edit label', group: 'Labels' },
    '/': { handler: () => document.getElementById('label-search')?.focus(), description: 'Search labels', group: 'Labels' },
  });
  useCommands(writable ? [{ id: 'labels.new', title: 'New label', group: 'Labels', icon: PlusIcon, shortcut: 'n', run: () => setCreating(true) }] : [], [writable]);

  return (
    <div className={styles.page}>
      <div className={styles.toolbar}>
        <Input
          id="label-search"
          className={styles.search}
          leadingIcon={SearchIcon}
          placeholder="Search all labels"
          value={filter}
          onChange={(e) => setQuery({ q: e.target.value || null })}
          onKeyDown={(e) => e.key === 'Escape' && (e.target as HTMLInputElement).blur()}
          aria-label="Search labels"
        />
        <Select value={sort} onChange={(e) => setQuery({ sort: e.target.value === 'name' ? null : e.target.value })} aria-label="Sort labels" className={styles.sort}>
          {SORTS.map((s) => (
            <option key={s.key} value={s.key}>
              Sort: {s.label}
            </option>
          ))}
        </Select>
        {writable && (
          <Button variant="primary" leadingIcon={PlusIcon} kbd="N" onClick={() => setCreating(true)}>
            New label
          </Button>
        )}
      </div>
      {creating && (
        <div className={styles.box}>
          <LabelForm
            repo={repo}
            onDone={() => setCreating(false)}
            onSubmit={(input) => {
              createLabel(repo, input);
              toast({ kind: 'success', title: `Created label ${input.name}` });
            }}
          />
        </div>
      )}
      <div className={styles.box}>
        <div className={styles.boxHeader}>
          <TagIcon size={16} /> {labels.length} label{labels.length === 1 ? '' : 's'}
        </div>
        {labels.length === 0 ? (
          <EmptyState icon={TagIcon} title={filter ? 'No labels match' : 'No labels yet'} />
        ) : (
          <ul className={styles.list} aria-label="Labels">
            {labels.map((l, i) =>
              editing === l.id ? (
                <li key={l.id} className={styles.row}>
                  <LabelForm
                    repo={repo}
                    label={l}
                    onDone={() => setEditing(null)}
                    onSubmit={(input) => updateLabel(l, input)}
                  />
                </li>
              ) : (
                <li key={l.id} className={cx(styles.row, i === cursor && styles.rowActive)} onMouseEnter={() => setActive(i)} aria-current={i === cursor}>
                  <span className={styles.name}>
                    <LabelPill label={l} />
                  </span>
                  <span className={styles.desc}>{l.description}</span>
                  <Link to={`${base}/issues?q=${encodeURIComponent(`is:open label:${q(l.name)}`)}`} className={styles.count}>
                    {(usage.get(l.id) ?? 0) > 0 && (
                      <>
                        <IssueOpenedIcon size={14} /> {usage.get(l.id)} open
                      </>
                    )}
                  </Link>
                  {writable && (
                    <span className={styles.actions}>
                      <Button size="sm" variant="ghost" disabled={l.id < 0} onClick={() => setEditing(l.id)}>
                        Edit
                      </Button>
                      <Button size="sm" variant="ghost" className={styles.danger} disabled={l.id < 0} onClick={() => setConfirm(l)}>
                        Delete
                      </Button>
                    </span>
                  )}
                </li>
              ),
            )}
          </ul>
        )}
      </div>
      <ConfirmDialog open={!!confirm} onClose={() => setConfirm(null)} onConfirm={() => confirm && deleteLabel(confirm)} title={`Delete label “${confirm?.name ?? ''}”?`}>
        This removes the label from every issue and pull request. This can’t be undone.
      </ConfirmDialog>
    </div>
  );
});

/** Create / edit form with live preview. Name must be unique (case-insensitive). */
const LabelForm = observer(function LabelForm({
  repo,
  label,
  onSubmit,
  onDone,
}: {
  repo: Repo;
  label?: Label;
  onSubmit: (input: { name: string; color: string; description: string | null }) => void;
  onDone: () => void;
}) {
  const [name, setName] = useState(label?.name ?? '');
  const [description, setDescription] = useState(label?.description ?? '');
  const [color, setColor] = useState(label?.color ?? randomLabelColor);
  const clash = labelByName(repo.id, name.trim());
  const nameError = clash && clash.id !== label?.id ? 'Name has already been taken' : null;
  const valid = name.trim() !== '' && !nameError && description.length <= 100;
  const submit = () => {
    if (!valid) return;
    onSubmit({ name: name.trim(), color, description: description.trim() || null });
    onDone();
  };
  return (
    <form
      className={styles.form}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.stopPropagation();
          onDone();
        }
      }}
    >
      <div className={styles.preview}>
        <LabelPill label={{ name: name.trim() || 'Label preview', color, description: null }} />
      </div>
      <div className={styles.fields}>
        <label className={styles.field}>
          <span>Label name</span>
          <Input autoFocus value={name} onChange={(e) => setName(e.target.value)} maxLength={50} invalid={!!nameError} placeholder="Label name" />
          {nameError && <span className={styles.error}>{nameError}</span>}
        </label>
        <label className={cx(styles.field, styles.grow)}>
          <span>Description</span>
          <Input value={description} onChange={(e) => setDescription(e.target.value)} maxLength={100} placeholder="Description (optional)" />
        </label>
        <div className={styles.field}>
          <span>Color</span>
          <ColorPicker value={color} onChange={setColor} />
        </div>
        <div className={styles.formActions}>
          <Button variant="ghost" onClick={onDone}>
            Cancel
          </Button>
          <Button type="submit" variant="primary" disabled={!valid}>
            {label ? 'Save changes' : 'Create label'}
          </Button>
        </div>
      </div>
    </form>
  );
});
