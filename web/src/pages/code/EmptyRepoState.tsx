import { navigate } from '../../router';
import type { Repo } from '../../sync/models';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { RepoIcon } from '../../ui/icons';

/** Neutral state for history/branch views of a repository with no commits yet. */
export function EmptyRepoState({ repo }: { repo: Pick<Repo, 'owner' | 'name'> }) {
  return (
    <EmptyState
      icon={RepoIcon}
      title="This repository is empty"
      action={
        <Button size="lg" onClick={() => navigate(`/${repo.owner}/${repo.name}`)}>
          Quick setup
        </Button>
      }
    >
      Push a first commit to see it here.
    </EmptyState>
  );
}
