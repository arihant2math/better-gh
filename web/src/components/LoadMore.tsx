import type { HTMLAttributes, ReactNode } from 'react';
import { useAutoLoad, type Pager } from '../api/pager';
import { Button } from '../ui/Button';
import { Spinner } from '../ui/Spinner';

interface Props extends HTMLAttributes<HTMLDivElement> {
  pager: Pick<Pager<unknown>, 'status' | 'loadMore' | 'retry'>;
  /** Infinite scroll: load when mounted and after each successful page. */
  auto?: boolean;
  label?: ReactNode;
  loadingLabel?: ReactNode;
  /** Hover/focus intent on the button (e.g. prefetch the next page). */
  onIntent?(): void;
}

/** Trailing "load more" row of a paged list, with loading, backoff and retry states. */
export function LoadMore({ pager, auto = false, label = 'Load more', loadingLabel = 'Loading more…', onIntent, ...rest }: Props) {
  const { status, loadMore, retry } = pager;
  useAutoLoad(pager, auto);
  return (
    <div {...rest} aria-live="polite">
      {status === 'loading' || status === 'backoff' ? (
        <>
          <Spinner size={14} /> {status === 'backoff' ? 'Retrying…' : loadingLabel}
        </>
      ) : status === 'error' ? (
        <>
          Couldn’t load more.{' '}
          <Button size="sm" onClick={retry}>
            Retry
          </Button>
        </>
      ) : (
        <Button size="sm" onClick={loadMore} onMouseEnter={onIntent} onFocus={onIntent}>
          {label}
        </Button>
      )}
    </div>
  );
}
