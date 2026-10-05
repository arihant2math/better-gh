import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { store } from '../../sync';
import type { Issue, ProjectField, ProjectValue, ProjectView } from '../../sync/models';
import { addDraftItem, addIssueItem, itemsForProject } from '../../sync/projects';
import { repoByName } from '../../sync/selectors';
import { StateIcon } from '../../ui/Badge';
import { PlusIcon } from '../../ui/icons';
import overlay from '../../ui/Overlay.module.css';
import { Popover } from '../../ui/Popover';
import { toast } from '../../ui/Toast';
import type { ProjectCtx } from './data';
import { parseFilter } from './query';
import styles from './Projects.module.css';

/** `#12`, `owner/repo#12` or a draft title. */
export function parseAddInput(
  text: string,
): { kind: 'issue'; owner?: string; repo?: string; number: number } | { kind: 'draft'; title: string } | { kind: 'search'; q: string } | null {
  const t = text.trim();
  if (!t) return null;
  let m = /^([\w.-]+)\/([\w.-]+)#(\d+)$/.exec(t);
  if (m) return { kind: 'issue', owner: m[1], repo: m[2], number: Number(m[3]) };
  m = /^#(\d+)$/.exec(t);
  if (m) return { kind: 'issue', number: Number(m[1]) };
  if (t.startsWith('#')) return { kind: 'search', q: t.slice(1).trim().toLowerCase() };
  return { kind: 'draft', title: t };
}

/** Field values implied by the view filter (e.g. `status:Todo`), applied to new drafts like GitHub does. */
function valuesFromFilter(view: ProjectView, fields: ProjectField[]): Record<string, ProjectValue> {
  const out: Record<string, ProjectValue> = {};
  for (const t of parseFilter(view.filter).terms) {
    if (t.negate || t.values.length !== 1) continue;
    const f = fields.find((x) => x.name.toLowerCase() === t.key && (x.dataType === 'status' || x.dataType === 'single_select'));
    const o = f?.options?.find((x) => x.name.toLowerCase() === t.values[0]!.toLowerCase());
    if (f && o) out[String(f.id)] = o.id;
  }
  return out;
}

export const AddItemRow = observer(function AddItemRow({
  ctx,
  view,
  fields,
  values,
  compact,
  inputRef,
}: {
  ctx: ProjectCtx;
  view: ProjectView;
  fields: ProjectField[];
  /** Extra values for new drafts (board column). */
  values?: Record<string, ProjectValue>;
  compact?: boolean;
  inputRef?: React.Ref<HTMLInputElement>;
}) {
  const [text, setText] = useState('');
  const [active, setActive] = useState(0);
  const anchor = useRef<HTMLDivElement>(null);
  const parsed = parseAddInput(text);
  const project = ctx.project;

  // Suggestions: issues of linked repositories (in the store) for `#…`.
  const suggestions: Issue[] = [];
  if (parsed && (parsed.kind === 'search' || (parsed.kind === 'issue' && !parsed.owner))) {
    const inProject = new Set(itemsForProject(project.id).map((i) => i.issueId));
    const q = parsed.kind === 'search' ? parsed.q : String(parsed.number);
    for (const repoId of project.linkedRepoIds) {
      for (const i of store().byIndex('issue', 'repoId', repoId)) {
        if (inProject.has(i.id)) continue;
        if (!q || String(i.number).startsWith(q) || i.title.toLowerCase().includes(q)) suggestions.push(i);
      }
    }
    suggestions.sort((a, b) => Number(b.state === 'open') - Number(a.state === 'open') || b.number - a.number);
    suggestions.splice(8);
  }

  const addIssue = (i: Issue) => {
    addIssueItem(project, { issueId: i.id, isPr: i.isPr }).done.catch(() => undefined);
    setText('');
  };

  const submit = () => {
    if (!parsed) return;
    if (suggestions.length && parsed.kind !== 'draft') {
      addIssue(suggestions[Math.min(active, suggestions.length - 1)]!);
      return;
    }
    if (parsed.kind === 'draft') {
      addDraftItem(project, parsed.title, { ...valuesFromFilter(view, fields), ...values });
    } else if (parsed.kind === 'issue') {
      const repo = parsed.owner
        ? repoByName(parsed.owner, parsed.repo!)
        : project.linkedRepoIds.length === 1
          ? store().get('repo', project.linkedRepoIds[0])
          : undefined;
      if (!repo && !parsed.owner) {
        toast({ kind: 'error', title: 'Which repository?', description: 'Use owner/repo#number, or link exactly one repository to the project.' });
        return;
      }
      const issue = repo ? store().byKey('issue', 'number', `${repo.id}#${parsed.number}`) : undefined;
      const { done } = issue
        ? addIssueItem(project, { issueId: issue.id, isPr: issue.isPr })
        : addIssueItem(project, { owner: parsed.owner ?? repo!.owner, repo: parsed.repo ?? repo!.name, number: parsed.number });
      done.catch(() => undefined);
    } else return;
    setText('');
  };

  return (
    <div ref={anchor} className={compact ? styles.addCompact : styles.addRow}>
      <PlusIcon size={16} />
      <input
        ref={inputRef}
        className={styles.addInput}
        value={text}
        placeholder={compact ? 'Add item' : 'Add item — type a title for a draft, or #123 / owner/repo#123 for an issue'}
        aria-label="Add item"
        onChange={(e) => {
          setText(e.target.value);
          setActive(0);
        }}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === 'Enter') submit();
          else if (e.key === 'Escape') {
            setText('');
            (e.target as HTMLInputElement).blur();
          } else if (e.key === 'ArrowDown' && suggestions.length) {
            e.preventDefault();
            setActive((a) => (a + 1) % suggestions.length);
          } else if (e.key === 'ArrowUp' && suggestions.length) {
            e.preventDefault();
            setActive((a) => (a - 1 + suggestions.length) % suggestions.length);
          }
        }}
      />
      <Popover open={suggestions.length > 0} onClose={() => setText('')} anchor={anchor} restoreFocus={false} className={overlay.panel}>
        <div className={overlay.panelList} role="listbox" aria-label="Issues">
          {suggestions.map((i, k) => (
            <button
              key={i.id}
              type="button"
              role="option"
              aria-selected={k === active}
              data-active={k === active}
              className={overlay.menuItem}
              onPointerDown={(e) => e.preventDefault()}
              onClick={() => addIssue(i)}
            >
              <StateIcon issue={i} size={14} />
              <span className={overlay.menuItemLabel}>
                {i.title}
                <span className={overlay.menuItemDesc}>
                  {store().get('repo', i.repoId)?.name}#{i.number}
                </span>
              </span>
            </button>
          ))}
        </div>
      </Popover>
    </div>
  );
});
