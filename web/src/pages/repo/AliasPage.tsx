import { useEffect } from 'react';
import { navigate, useLocation, useMatch } from '../../router';
import { Spinner } from '../../ui/Spinner';
import { aliasTarget, type AliasKind } from './redirects';

const KINDS: Record<string, AliasKind> = {
  '/:owner/:repo/labels/:name': 'label',
  '/:owner/:repo/search': 'repo-search',
  '/orgs/:org/people': 'org-people',
  '/orgs/:org/repositories': 'org-repositories',
  '/orgs/:org/teams': 'org-teams',
};

/** Replaces an alias URL (`html_url` shapes) with the page that shows it. */
export default function AliasPage() {
  const match = useMatch();
  const { search } = useLocation();
  const kind = match ? KINDS[match.route.path] : undefined;
  useEffect(() => {
    if (match && kind) navigate(aliasTarget(kind, match.params, search), { replace: true });
  }, [match, kind, search]);
  return <Spinner />;
}
