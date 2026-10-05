import { observer } from 'mobx-react-lite';
import { IconButton } from '../ui/Button';
import { AlertIcon, MegaphoneIcon, ToolsIcon, XIcon } from '../ui/icons';
import { formatRelative } from '../ui/RelativeTime';
import styles from './SiteBanners.module.css';
import type { SiteState } from './site';


/** One announcement banner (also used as the live preview in site admin). */
export function AnnouncementBanner({ message, dismissible, onDismiss }: { message: string; dismissible?: boolean; onDismiss?: () => void }) {
  return (
    <div className={styles.banner} data-kind="announcement" role="region" aria-label="Announcement">
      <MegaphoneIcon size={14} className={styles.icon} />
      <span className={styles.text}>{message}</span>
      {dismissible && <IconButton icon={XIcon} label="Dismiss announcement" size="sm" tooltip={false} onClick={onDismiss} />}
    </div>
  );
}

export function MaintenanceBanner({ enabled, message, scheduledAt }: { enabled: boolean; message: string | null; scheduledAt: string | null }) {
  const upcoming = !enabled && scheduledAt && Date.parse(scheduledAt) > Date.now();
  if (!enabled && !upcoming) return null;
  return (
    <div className={styles.banner} data-kind="maintenance" role="status">
      {enabled ? <ToolsIcon size={14} className={styles.icon} /> : <AlertIcon size={14} className={styles.icon} />}
      <span className={styles.text}>
        <strong>{enabled ? 'Maintenance mode is on.' : `Maintenance scheduled ${formatRelative(scheduledAt!)}.`}</strong>{' '}
        {message || (enabled ? 'Only site administrators can use this instance right now.' : '')}
      </span>
    </div>
  );
}

/** App-wide banners (announcement + maintenance); lazily loaded by the shell when one is active. */
const SiteBanners = observer(function SiteBanners({ site }: { site: SiteState }) {
  const info = site.info;
  if (!info) return null;
  const a = site.announcement;
  return (
    <>
      <MaintenanceBanner enabled={info.maintenance.enabled} message={info.maintenance.message} scheduledAt={info.maintenance.scheduled_at} />
      {a && <AnnouncementBanner message={a.message} dismissible={a.user_dismissible} onDismiss={() => site.dismissAnnouncement()} />}
    </>
  );
});

export default SiteBanners;
