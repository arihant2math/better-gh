import { Button } from '../ui/Button';
import { EmptyState } from '../ui/EmptyState';
import { AlertIcon } from '../ui/icons';
import { navigate } from '../router';

export function NotFound({ what = 'page' }: { what?: string }) {
  return (
    <EmptyState
      icon={AlertIcon}
      title={`This ${what} could not be found`}
      action={
        <Button variant="secondary" onClick={() => navigate('/')}>
          Go home
        </Button>
      }
    >
      It may have been moved or deleted, or you may not have access to it.
    </EmptyState>
  );
}
