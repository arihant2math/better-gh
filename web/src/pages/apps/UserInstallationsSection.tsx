import { observer } from 'mobx-react-lite';
import { session } from '../../app/session';
import { useLocation } from '../../router';
import { subPath } from '../settings/developer/common';
import { InstallationsManager } from './InstallationsManager';

/** `/settings/installations[/:id]`: GitHub Apps installed on the viewer's account. */
export default observer(function UserInstallationsSection() {
  const { pathname } = useLocation();
  const login = session.user?.login;
  if (!login) return null;
  return <InstallationsManager account={login} base="/settings/installations" sub={subPath(pathname)} />;
});
