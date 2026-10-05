import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { invalidate } from '../../api/cache';
import { ApiError } from '../../api/client';
import { deleteRuleset, getRuleset, listRulesets, scopeKey, type Ruleset, type RulesetTarget } from '../../api/rulesets';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, downloadText, errorMessage } from '../../components/settings/kit';
import { Link, navigate } from '../../router';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { GitBranchIcon, KebabHorizontalIcon, PlusIcon, RepoIcon, ShieldLockIcon, TagIcon, TriangleDownIcon, UploadIcon, type Icon } from '../../ui/icons';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { ListSkeleton, LoadError, useLocalResource } from '../repo-settings/shared';
import { ENFORCEMENT_LABEL, TARGET_LABEL, exportRuleset, importRuleset } from './model';
import styles from './Rulesets.module.css';
import type { RulesetsHost } from './RulesetsSection';
import { setPendingImport } from './stash';

export const TARGET_ICON: Record<RulesetTarget, Icon> = { branch: GitBranchIcon, tag: TagIcon, push: RepoIcon };

export const enforcementTone = (e: Ruleset['enforcement']) => (e === 'active' ? 'success' : e === 'evaluate' ? 'warning' : 'neutral');

/** Settings path of an organization ruleset (for rulesets a repository inherits). */
const orgRulesetHref = (org: string, id: number) => `/organizations/${encodeURIComponent(org)}/settings/rules/${id}`;

export const RulesetList = observer(function RulesetList({ host }: { host: RulesetsHost }) {
  const { scope, base } = host;
  const key = scopeKey(scope, 'list');
  const list = useLocalResource(key, () => listRulesets(scope));
  const [menu, setMenu] = useState(false);
  const [deleting, setDeleting] = useState<Ruleset | null>(null);
  const newBtn = useRef<HTMLButtonElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const own = (r: Ruleset) => (scope.kind === 'org' ? true : r.source_type === 'Repository');

  const onImport = async (f: File | undefined) => {
    if (!f) return;
    try {
      const form = importRuleset(await f.text());
      setPendingImport(form);
      navigate(`${base}/new?target=${form.target}&import=1`);
    } catch (e) {
      toast({ kind: 'error', title: 'Could not import the ruleset', description: errorMessage(e) });
    } finally {
      if (file.current) file.current.value = '';
    }
  };

  const exportOne = async (r: Ruleset) => {
    try {
      const full = await getRuleset(scope, r.id);
      downloadText(`${r.name.replace(/[^\w.-]+/g, '-') || 'ruleset'}.json`, exportRuleset(full));
    } catch (e) {
      toast({ kind: 'error', title: 'Could not export the ruleset', description: errorMessage(e) });
    }
  };

  const items: MenuEntry[] = [
    { id: 'branch', label: 'New branch ruleset', icon: GitBranchIcon, onSelect: () => navigate(`${base}/new?target=branch`) },
    { id: 'tag', label: 'New tag ruleset', icon: TagIcon, onSelect: () => navigate(`${base}/new?target=tag`) },
    { id: 'push', label: 'New push ruleset', icon: RepoIcon, onSelect: () => navigate(`${base}/new?target=push`) },
    { separator: true, id: 'sep' },
    { id: 'import', label: 'Import a ruleset', icon: UploadIcon, onSelect: () => file.current?.click() },
  ];

  return (
    <>
      <PageHeader
        title="Rulesets"
        description={
          scope.kind === 'org'
            ? 'Rulesets apply to the repositories of this organization you select, on top of their own rules.'
            : 'Rulesets protect branches and tags matching patterns: who can push, what checks must pass and which commits are accepted.'
        }
        actions={
          host.readOnly ? undefined : (
            <>
              <Button ref={newBtn} variant="primary" leadingIcon={PlusIcon} trailingIcon={TriangleDownIcon} onClick={() => setMenu(true)} aria-haspopup="menu">
                New ruleset
              </Button>
              <Menu open={menu} onClose={() => setMenu(false)} anchor={newBtn} items={items} placement="bottom-end" aria-label="New ruleset" />
              <input
                ref={file}
                type="file"
                accept="application/json,.json"
                className={styles.hiddenInput}
                aria-label="Import a ruleset file"
                onChange={(e) => void onImport(e.target.files?.[0])}
              />
            </>
          )
        }
      />
      {list.error ? (
        isNotFound(list.error) ? (
          <Unavailable what={scope.kind === 'org' ? 'Organization rulesets are' : 'Rulesets are'} />
        ) : (
          <LoadError error={list.error} />
        )
      ) : null}
      {!list.data ? (
        list.error ? null : (
          <ListSkeleton rows={3} />
        )
      ) : list.data.length === 0 ? (
        <EmptyState icon={ShieldLockIcon} title="You haven't created any rulesets">
          Define whether collaborators can delete or force push and set requirements for any pushes, such as passing status checks or a linear commit history.
        </EmptyState>
      ) : (
        <ItemList aria-label="Rulesets">
          {list.data.map((r) => {
            const href = own(r) ? `${base}/${r.id}` : orgRulesetHref(r.source, r.id);
            return (
              <ItemRow
                key={r.id}
                icon={TARGET_ICON[r.target] ?? ShieldLockIcon}
                title={<Link to={href}>{r.name}</Link>}
                meta={
                  <span className={styles.meta}>
                    <span>{TARGET_LABEL[r.target]} ruleset</span>
                    {r.updated_at && (
                      <>
                        · <span>Updated</span> <RelativeTime date={r.updated_at} />
                      </>
                    )}
                    {!own(r) && <>· Managed by {r.source}</>}
                  </span>
                }
                actions={
                  <RowActions
                    r={r}
                    own={own(r)}
                    readOnly={!!host.readOnly}
                    onEdit={() => navigate(href)}
                    onExport={() => void exportOne(r)}
                    onDelete={() => setDeleting(r)}
                  />
                }
              />
            );
          })}
        </ItemList>
      )}
      {host.readOnly && <Banner tone="info">You can view these rulesets but not change them.</Banner>}
      <ConfirmDialog
        open={!!deleting}
        onClose={() => setDeleting(null)}
        title="Delete ruleset?"
        confirmLabel="Delete"
        onConfirm={async () => {
          if (!deleting) return;
          await deleteRuleset(scope, deleting.id);
          list.update((l) => l.filter((x) => x.id !== deleting.id));
          invalidate(scopeKey(scope, 'one'));
          toast({ kind: 'success', title: `Ruleset ${deleting.name} deleted` });
        }}
      >
        <p className={styles.muted}>
          Are you sure you want to delete <strong>{deleting?.name}</strong>? This action cannot be undone.
        </p>
      </ConfirmDialog>
    </>
  );
});

function RowActions({
  r,
  own,
  readOnly,
  onEdit,
  onExport,
  onDelete,
}: {
  r: Ruleset;
  own: boolean;
  readOnly: boolean;
  onEdit: () => void;
  onExport: () => void;
  onDelete: () => void;
}) {
  const [open, setOpen] = useState(false);
  const btn = useRef<HTMLButtonElement>(null);
  const items: MenuEntry[] = [
    { id: 'edit', label: own && !readOnly ? 'Edit ruleset' : 'View ruleset', onSelect: onEdit },
    { id: 'export', label: 'Export ruleset', onSelect: onExport },
  ];
  if (own && !readOnly) items.push({ separator: true, id: 'sep' }, { id: 'delete', label: 'Delete ruleset', danger: true, onSelect: onDelete });
  return (
    <>
      <Pill tone={enforcementTone(r.enforcement)}>{ENFORCEMENT_LABEL[r.enforcement]}</Pill>
      <IconButton ref={btn} icon={KebabHorizontalIcon} label={`Actions for ${r.name}`} size="sm" onClick={() => setOpen(true)} />
      <Menu open={open} onClose={() => setOpen(false)} anchor={btn} items={items} placement="bottom-end" />
    </>
  );
}

export const isNotFound = (e: unknown) => e instanceof ApiError && e.status === 404;

/** The server has no such endpoint (an older bgh-server). */
export function Unavailable({ what }: { what: string }) {
  return (
    <EmptyState icon={ShieldLockIcon} title={`${what} not available`}>
      This server does not support this part of the rulesets API yet.
    </EmptyState>
  );
}
