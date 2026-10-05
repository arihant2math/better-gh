import { observer } from 'mobx-react-lite';
import { session } from '../../app/session';
import { theme, type ThemePref } from '../../app/theme';
import { Link, useParams } from '../../router';
import { MODEL_NAMES } from '../../sync/schema';
import { store, sync } from '../../sync';
import { Avatar } from '../../ui/Badge';
import { Button, cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { BellIcon, GearIcon, KeyIcon, MoonIcon, PersonIcon, ShieldIcon, SyncIcon, type Icon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import styles from './SettingsPage.module.css';

const SECTIONS: { id: string; label: string; icon: Icon }[] = [
  { id: 'profile', label: 'Public profile', icon: PersonIcon },
  { id: 'appearance', label: 'Appearance', icon: MoonIcon },
  { id: 'notifications', label: 'Notifications', icon: BellIcon },
  { id: 'keys', label: 'SSH and GPG keys', icon: KeyIcon },
  { id: 'security', label: 'Password and authentication', icon: ShieldIcon },
  { id: 'developer', label: 'Developer settings', icon: GearIcon },
  { id: 'local', label: 'Local data & sync', icon: SyncIcon },
];

export default observer(function SettingsPage() {
  const { section = 'profile' } = useParams<{ section?: string }>();
  const current = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0]!;
  return (
    <div className={styles.page}>
      <nav className={styles.nav} aria-label="Settings">
        {SECTIONS.map((s) => (
          <Link key={s.id} to={`/settings/${s.id}`} className={styles.navItem} aria-current={s.id === current.id ? 'page' : undefined}>
            <s.icon size={16} />
            {s.label}
          </Link>
        ))}
      </nav>
      <div className={styles.content}>
        <h1 className={styles.title}>{current.label}</h1>
        {current.id === 'profile' ? <Profile /> : current.id === 'appearance' ? <Appearance /> : current.id === 'local' ? <LocalData /> : <Placeholder />}
      </div>
    </div>
  );
});

const Profile = observer(function Profile() {
  const u = session.user!;
  return (
    <div className={styles.form}>
      <div className={styles.avatarRow}>
        <Avatar user={u} size={64} />
        <Button size="sm">Upload new picture</Button>
      </div>
      <Field label="Name" htmlFor="name">
        <Input id="name" defaultValue={u.name ?? ''} />
      </Field>
      <Field label="Username" htmlFor="login" hint="Changing your username can have unintended side effects.">
        <Input id="login" defaultValue={u.login} disabled />
      </Field>
      <div>
        <Button variant="primary">Update profile</Button>
      </div>
    </div>
  );
});

const THEMES: { id: ThemePref; label: string; desc: string }[] = [
  { id: 'system', label: 'Sync with system', desc: 'Follow your operating system setting' },
  { id: 'light', label: 'Light', desc: 'Bright and crisp' },
  { id: 'dark', label: 'Dark', desc: 'Easy on the eyes at night' },
];

const Appearance = observer(function Appearance() {
  return (
    <div className={styles.themes} role="radiogroup" aria-label="Theme">
      {THEMES.map((t) => (
        <button key={t.id} type="button" role="radio" aria-checked={theme.pref === t.id} className={cx(styles.themeCard)} onClick={() => theme.set(t.id)}>
          <span className={cx(styles.swatch, styles[`swatch-${t.id}`])} />
          <strong>{t.label}</strong>
          <span className={styles.desc}>{t.desc}</span>
        </button>
      ))}
    </div>
  );
});

const LocalData = observer(function LocalData() {
  const c = sync();
  const s = store();
  return (
    <div className={styles.form}>
      <p className={styles.desc}>
        Everything you see is served from a local database kept in sync over a WebSocket. Mutations apply instantly and are sent in the background.
      </p>
      <dl className={styles.stats}>
        <dt>Status</dt>
        <dd>{c.status}</dd>
        <dt>Last sync id</dt>
        <dd>{c.lastSyncId}</dd>
        <dt>Pending changes</dt>
        <dd>{c.queue.pendingCount}</dd>
        <dt>Subscribed scopes</dt>
        <dd>{c.scopes.size}</dd>
        {MODEL_NAMES.map((m) => (
          <span key={m} style={{ display: 'contents' }}>
            <dt>{m}</dt>
            <dd>{s.count(m).toLocaleString()}</dd>
          </span>
        ))}
      </dl>
      <div>
        <Button
          variant="danger"
          onClick={async () => {
            await c.bootstrap();
          }}
        >
          Re-download workspace
        </Button>
      </div>
    </div>
  );
});

function Placeholder() {
  return <EmptyState icon={GearIcon} title="Coming soon">This settings page has not been built yet.</EmptyState>;
}
