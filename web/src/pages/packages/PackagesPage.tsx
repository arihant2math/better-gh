import { observer } from 'mobx-react-lite';
import { Link, useParams } from '../../router';
import { orgByLogin, userByLogin } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { PackageList } from './PackageList';
import styles from './Packages.module.css';

/** `/orgs/:owner/packages` and `/users/:owner/packages` — an owner's packages. */
export default observer(function PackagesPage() {
  const { owner } = useParams<{ owner: string }>();
  const org = orgByLogin(owner);
  const user = org ? undefined : userByLogin(owner);
  const login = org?.login ?? user?.login ?? owner;
  return (
    <div className={styles.page}>
      <header className={styles.ownerHeader}>
        <Avatar user={{ login, avatarUrl: org?.avatarUrl ?? user?.avatarUrl ?? '', name: org?.name ?? user?.name ?? null }} size={40} square={!!org} />
        <h1>
          <Link to={`/${login}`}>{login}</Link>
        </h1>
      </header>
      <PackageList owner={login} />
    </div>
  );
});
