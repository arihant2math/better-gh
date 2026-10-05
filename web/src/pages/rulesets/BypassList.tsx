import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import type { BypassActor, BypassMode } from '../../api/rulesets';
import { getUser } from '../../api/repoSettings';
import { errorMessage } from '../../components/settings/kit';
import { store } from '../../sync';
import type { ID } from '../../sync/models';
import { Button, IconButton } from '../../ui/Button';
import { AppsIcon, KeyIcon, OrganizationIcon, PeopleIcon, PersonIcon, PlusIcon, ShieldIcon, XIcon, type Icon } from '../../ui/icons';
import { Select } from '../../ui/Input';
import { SelectPanel, type SelectItem } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import type { AppRef } from './data';
import { REPO_ROLES } from './model';
import styles from './Rulesets.module.css';

const key = (a: Pick<BypassActor, 'actor_type' | 'actor_id'>) => `${a.actor_type}:${a.actor_type === 'DeployKey' ? '' : (a.actor_id ?? '')}`;

const ICON: Record<BypassActor['actor_type'], Icon> = {
  RepositoryRole: ShieldIcon,
  OrganizationAdmin: OrganizationIcon,
  Team: PeopleIcon,
  User: PersonIcon,
  Integration: AppsIcon,
  DeployKey: KeyIcon,
};

/** Display name of a bypass actor. */
export function actorLabel(a: BypassActor, apps: AppRef[] = []): string {
  switch (a.actor_type) {
    case 'RepositoryRole':
      return REPO_ROLES.find((r) => r.id === a.actor_id)?.name ?? (a.actor_id === 1 ? 'Read' : a.actor_id === 3 ? 'Triage' : `Repository role #${a.actor_id}`);
    case 'OrganizationAdmin':
      return 'Organization admin';
    case 'DeployKey':
      return 'Deploy keys';
    case 'Team': {
      const t = a.actor_id ? store().get('team', a.actor_id) : undefined;
      return t ? t.name : `Team #${a.actor_id}`;
    }
    case 'User': {
      const u = a.actor_id ? store().get('user', a.actor_id) : undefined;
      return u ? u.login : `User #${a.actor_id}`;
    }
    case 'Integration':
      return apps.find((x) => x.id === a.actor_id)?.name ?? `App #${a.actor_id}`;
  }
}

const KIND_LABEL: Record<BypassActor['actor_type'], string> = {
  RepositoryRole: 'Role',
  OrganizationAdmin: 'Role',
  Team: 'Team',
  User: 'User',
  Integration: 'App',
  DeployKey: 'Deploy key',
};

/** The ruleset's bypass list with an actor picker (roles, org admins, teams, apps, deploy keys, users). */
export const BypassList = observer(function BypassList({
  value,
  onChange,
  orgId,
  apps,
  error,
}: {
  value: BypassActor[];
  onChange: (v: BypassActor[]) => void;
  /** Organization owning the ruleset or repository (enables org admin and teams). */
  orgId: ID | null;
  apps: AppRef[];
  error?: string;
}) {
  const [open, setOpen] = useState(false);
  const btn = useRef<HTMLButtonElement>(null);
  const has = new Set(value.map(key));
  const candidates: { actor: Omit<BypassActor, 'bypass_mode'>; text: string; description: string }[] = [
    ...REPO_ROLES.map((r) => ({ actor: { actor_type: 'RepositoryRole' as const, actor_id: r.id }, text: r.name, description: 'Role' })),
    ...(orgId ? [{ actor: { actor_type: 'OrganizationAdmin' as const, actor_id: 1 }, text: 'Organization admin', description: 'Role' }] : []),
    { actor: { actor_type: 'DeployKey' as const, actor_id: null }, text: 'Deploy keys', description: 'Pushes made with a deploy key' },
    ...(orgId ? store().byIndex('team', 'orgId', orgId) : [])
      .slice()
      .sort((a, b) => a.name.localeCompare(b.name))
      .map((t) => ({ actor: { actor_type: 'Team' as const, actor_id: t.id }, text: t.name, description: `Team @${t.slug}` })),
    ...apps.map((a) => ({ actor: { actor_type: 'Integration' as const, actor_id: a.id }, text: a.name, description: `App ${a.slug} (ID ${a.id})` })),
  ];
  const items: SelectItem[] = candidates.map((c) => ({ id: key(c.actor), text: c.text, description: c.description, selected: has.has(key(c.actor)) }));

  const toggle = (id: SelectItem['id']) => {
    if (has.has(String(id))) onChange(value.filter((a) => key(a) !== id));
    else {
      const c = candidates.find((x) => key(x.actor) === id);
      if (c) onChange([...value, { ...c.actor, bypass_mode: 'always' }]);
    }
  };

  const create = async (q: string) => {
    const t = q.trim().replace(/^@/, '');
    if (/^\d+$/.test(t)) {
      const actor: BypassActor = { actor_type: 'Integration', actor_id: Number(t), bypass_mode: 'always' };
      if (!has.has(key(actor))) onChange([...value, actor]);
      return;
    }
    try {
      const u = await getUser(t);
      if (u.type !== 'User') throw new Error(`${u.login} is not a user.`);
      const actor: BypassActor = { actor_type: 'User', actor_id: u.id, bypass_mode: 'always' };
      if (!has.has(key(actor))) onChange([...value, actor]);
    } catch (e) {
      toast({ kind: 'error', title: `Could not add @${t}`, description: errorMessage(e) });
    }
  };

  return (
    <div className={styles.box}>
      <div className={styles.boxHead}>
        <span>Bypass list</span>
        <span className={styles.spacer} />
        <Button ref={btn} size="sm" leadingIcon={PlusIcon} onClick={() => setOpen(true)} aria-haspopup="dialog">
          Add bypass
        </Button>
        <SelectPanel
          open={open}
          onClose={() => setOpen(false)}
          anchor={btn}
          title="Add bypass"
          placeholder="Filter, or type a username or app ID"
          placement="bottom-end"
          items={items}
          onToggle={toggle}
          onCreate={(q) => void create(q)}
          createLabel={(q) => (/^\d+$/.test(q.trim()) ? `Add app with ID ${q.trim()}` : `Add user @${q.trim().replace(/^@/, '')}`)}
        />
      </div>
      {value.length === 0 ? (
        <div className={styles.boxEmpty}>Bypass list is empty. Nobody can skip these rules.</div>
      ) : (
        value.map((a) => {
          const I = ICON[a.actor_type];
          const label = actorLabel(a, apps);
          return (
            <div key={key(a)} className={styles.boxRow}>
              <I size={16} />
              <span>{label}</span>
              <span className={styles.muted}>{KIND_LABEL[a.actor_type]}</span>
              <span className={styles.spacer} />
              <Select
                aria-label={`Bypass mode for ${label}`}
                value={a.bypass_mode}
                onChange={(e) => onChange(value.map((x) => (key(x) === key(a) ? { ...x, bypass_mode: e.target.value as BypassMode } : x)))}
              >
                <option value="always">Always allow</option>
                <option value="pull_request">For pull requests only</option>
                <option value="exempt">Exempt</option>
              </Select>
              <IconButton icon={XIcon} size="sm" label={`Remove ${label} from the bypass list`} onClick={() => onChange(value.filter((x) => key(x) !== key(a)))} />
            </div>
          );
        })
      )}
      {error && <div className={styles.boxEmpty} style={{ color: 'var(--danger)' }}>{error}</div>}
    </div>
  );
});
