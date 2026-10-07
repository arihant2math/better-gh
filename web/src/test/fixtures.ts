import type { SimpleUser } from '../api/types';

/** A complete `simple-user` for test fixtures. */
export const simpleUser = (login: string, id: number, extra: Partial<SimpleUser> = {}): SimpleUser => ({
  login,
  id,
  node_id: `U_${id}`,
  avatar_url: `/avatars/${id}`,
  html_url: `/${login}`,
  type: 'User',
  site_admin: false,
  ...extra,
});
