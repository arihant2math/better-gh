import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { Link } from '../../router';
import { store } from '../../sync';
import { useIssueDetails } from '../../sync/hooks';
import type { ProjectField } from '../../sync/models';
import { deleteItem, updateItem } from '../../sync/projects';
import { StateBadge } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Skeleton } from '../../ui/EmptyState';
import { ArchiveIcon, LinkExternalIcon, PencilIcon, TrashIcon } from '../../ui/icons';
import { Input, Textarea } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import type { ProjectCtx } from './data';
import { FieldCell, KindIcon } from './FieldCell';
import { resolveRow } from './fields';
import styles from './Projects.module.css';

export const ItemPanel = observer(function ItemPanel({
  ctx,
  itemId,
  fields,
  onClose,
}: {
  ctx: ProjectCtx;
  itemId: number;
  fields: ProjectField[];
  onClose: () => void;
}) {
  const item = store().get('projectItem', itemId);
  return (
    <Dialog open onClose={onClose} className={styles.sheet} title={item ? undefined : 'Item'} aria-label="Item details">
      {item ? <ItemBody ctx={ctx} itemId={itemId} fields={fields} onClose={onClose} /> : <p className={styles.muted}>This item no longer exists.</p>}
    </Dialog>
  );
});

const ItemBody = observer(function ItemBody({
  ctx,
  itemId,
  fields,
  onClose,
}: {
  ctx: ProjectCtx;
  itemId: number;
  fields: ProjectField[];
  onClose: () => void;
}) {
  const item = store().get('projectItem', itemId)!;
  const row = resolveRow(item, ctx);
  const issue = row.issue;
  const inStore = !!issue && ctx.issueInStore(issue.id);
  const loaded = useIssueDetails(inStore ? issue!.id : undefined);
  const [editingTitle, setEditingTitle] = useState<string | null>(null);
  const [editingBody, setEditingBody] = useState<string | null>(null);
  const isDraft = row.kind === 'draft';
  const href = issue && row.repo ? `/${row.repo.owner}/${row.repo.name}/${issue.isPr ? 'pull' : 'issues'}/${issue.number}` : null;
  const draftBody = isDraft ? ctx.draftBody(item.id) : undefined;

  return (
    <div className={styles.panel}>
      <header className={styles.panelHead}>
        <div className={styles.panelMeta}>
          <KindIcon row={row} />
          {isDraft
            ? 'Draft issue'
            : row.repo
              ? `${row.repo.owner}/${row.repo.name} #${issue!.number}`
              : issue
                ? `#${issue.number}`
                : 'You do not have access to this item, or it was deleted'}
          {item.archived && <span className={styles.tag}>Archived</span>}
        </div>
        {editingTitle !== null ? (
          <form
            className={styles.panelTitleForm}
            onSubmit={(e) => {
              e.preventDefault();
              if (editingTitle.trim()) updateItem(ctx.project, item, { title: editingTitle.trim() }, 'Rename draft');
              setEditingTitle(null);
            }}
          >
            <Input autoFocus value={editingTitle} onChange={(e) => setEditingTitle(e.target.value)} aria-label="Title" />
            <Button type="submit" variant="primary" size="sm">
              Save
            </Button>
            <Button size="sm" onClick={() => setEditingTitle(null)}>
              Cancel
            </Button>
          </form>
        ) : (
          <h2 className={styles.panelTitle}>
            {row.title}
            {isDraft && ctx.canWrite && <IconButton icon={PencilIcon} label="Edit title" size="sm" onClick={() => setEditingTitle(row.title)} />}
          </h2>
        )}
        {issue && (
          <div className={styles.panelState}>
            <StateBadge issue={issue} />
            {href && (
              <Link to={href} onClick={onClose} className={styles.panelLink}>
                Open {issue.isPr ? 'pull request' : 'issue'} <LinkExternalIcon size={14} />
              </Link>
            )}
          </div>
        )}
      </header>
      <div className={styles.panelGrid}>
        <div className={styles.panelMain}>
          {isDraft ? (
            editingBody !== null ? (
              <div className={styles.panelEditor}>
                <Textarea autoFocus rows={10} value={editingBody} onChange={(e) => setEditingBody(e.target.value)} aria-label="Body" />
                <div className={styles.rowEnd}>
                  <Button size="sm" onClick={() => setEditingBody(null)}>
                    Cancel
                  </Button>
                  <Button
                    size="sm"
                    variant="primary"
                    onClick={() => {
                      updateItem(ctx.project, item, { body: editingBody }, 'Edit draft');
                      setEditingBody(null);
                    }}
                  >
                    Save
                  </Button>
                </div>
              </div>
            ) : (
              <>
                <Markdown source={draftBody ?? ''} />
                {ctx.canWrite && (
                  <Button size="sm" leadingIcon={PencilIcon} onClick={() => setEditingBody(draftBody ?? '')}>
                    Edit description
                  </Button>
                )}
              </>
            )
          ) : inStore ? (
            loaded || issue!.body !== undefined ? (
              <Markdown source={issue!.body ?? ''} repo={row.repo ? `${row.repo.owner}/${row.repo.name}` : undefined} />
            ) : (
              <div className={styles.skeletons}>
                <Skeleton width="90%" />
                <Skeleton width="70%" />
                <Skeleton width="80%" />
              </div>
            )
          ) : (
            <p className={styles.muted}>{href ? 'Open the issue to read its description.' : 'Details are not available.'}</p>
          )}
        </div>
        <aside className={styles.panelFields} aria-label="Fields">
          {fields
            .filter((f) => f.dataType !== 'title')
            .map((f) => (
              <div key={f.id} className={styles.panelField}>
                <div className={styles.panelFieldName}>{f.name}</div>
                <div className={styles.panelFieldValue}>
                  <FieldCell row={row} field={f} ctx={ctx} />
                </div>
              </div>
            ))}
          {ctx.canWrite && item.id > 0 && (
            <div className={styles.panelActions}>
              <Button
                size="sm"
                leadingIcon={ArchiveIcon}
                onClick={() => updateItem(ctx.project, item, { archived: !item.archived }, item.archived ? 'Restore item' : 'Archive item')}
              >
                {item.archived ? 'Restore' : 'Archive'}
              </Button>
              <Button
                size="sm"
                variant="danger"
                leadingIcon={TrashIcon}
                onClick={() => {
                  deleteItem(ctx.project, item);
                  onClose();
                }}
              >
                Delete from project
              </Button>
            </div>
          )}
        </aside>
      </div>
    </div>
  );
});
