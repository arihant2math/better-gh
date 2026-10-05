import { observer } from 'mobx-react-lite';
import { session } from '../../app/session';
import { useLocation } from '../../router';
import { subPath } from '../settings/developer/common';
import { AppsManager } from './AppsManager';

/** `/settings/apps[/new|/:slug]`: GitHub Apps owned by the viewer. */
export default observer(function UserAppsSection() {
  const { pathname } = useLocation();
  const login = session.user?.login;
  if (!login) return null;
  return <AppsManager owner={login} base="/settings/apps" sub={subPath(pathname)} />;
});
