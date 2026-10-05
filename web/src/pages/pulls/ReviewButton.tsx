import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Issue } from '../../sync/models';
import { discardPendingReview, submitReview, type ReviewEvent } from '../../sync/pullMutations';
import { pendingComments, pendingReview } from '../../sync/pullSelectors';
import { Button } from '../../ui/Button';
import { ChevronDownIcon, CodeReviewIcon } from '../../ui/icons';
import { Textarea } from '../../ui/Input';
import { Popover } from '../../ui/Popover';
import { toast } from '../../ui/Toast';
import styles from './Review.module.css';

/** "Review changes" / "Finish your review": submit the pending review as Comment / Approve / Request changes. */
export const ReviewButton = observer(function ReviewButton({ pr }: { pr: Issue; repo: string }) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState('');
  const [event, setEvent] = useState<ReviewEvent>('COMMENT');
  const ref = useRef<HTMLButtonElement>(null);
  const pending = pendingReview(pr.id);
  const count = pendingComments(pr.id).length;
  const own = pr.authorId === store().viewerId;
  const closed = pr.state !== 'open';

  useShortcuts('Review', {
    'shift+r': { handler: () => setOpen(true), description: 'Review changes', group: 'Pull request' },
  });

  const submit = () => {
    if (event === 'COMMENT' && !body.trim() && count === 0) {
      toast({ kind: 'error', title: 'Add a comment or an inline comment to submit a review' });
      return;
    }
    submitReview(pr, event, body.trim()).done.then(
      () => toast({ kind: 'success', title: event === 'APPROVE' ? 'Approved' : event === 'REQUEST_CHANGES' ? 'Changes requested' : 'Review submitted' }),
      () => undefined,
    );
    setBody('');
    setEvent('COMMENT');
    setOpen(false);
  };

  const option = (value: ReviewEvent, label: string, hint: string, disabled = false) => (
    <label className={styles.radio} aria-disabled={disabled}>
      <input type="radio" name="review-event" value={value} checked={event === value} disabled={disabled} onChange={() => setEvent(value)} />
      <span>{label}</span>
      <small>{hint}</small>
    </label>
  );

  return (
    <>
      <Button ref={ref} size="sm" variant={pending ? 'primary' : 'secondary'} leadingIcon={CodeReviewIcon} trailingIcon={ChevronDownIcon} onClick={() => setOpen((o) => !o)} aria-haspopup="dialog">
        {pending ? 'Finish your review' : 'Review changes'}
        {count > 0 && <span className={styles.pendingBadge}>{count}</span>}
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={ref} placement="bottom-end" className={styles.reviewPanel} role="dialog" aria-label="Submit review">
        <h3>{pending ? 'Finish your review' : 'Review changes'}</h3>
        <Textarea
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Leave a comment"
          rows={4}
          autoFocus
          aria-label="Review summary"
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              submit();
            }
          }}
        />
        {option('COMMENT', 'Comment', 'Submit general feedback without explicit approval.')}
        {option('APPROVE', 'Approve', own ? 'Pull request authors can’t approve their own pull request.' : 'Give your approval to merge these changes.', own || closed)}
        {option('REQUEST_CHANGES', 'Request changes', own ? 'Pull request authors can’t request changes on their own pull request.' : 'Submit feedback that must be addressed before merging.', own || closed)}
        <div className={styles.reviewActions}>
          {pending && (
            <Button
              variant="ghost"
              onClick={() => {
                discardPendingReview(pr);
                setOpen(false);
              }}
            >
              Discard review{count ? ` (${count})` : ''}
            </Button>
          )}
          <Button variant="primary" onClick={submit}>
            Submit review
          </Button>
        </div>
      </Popover>
    </>
  );
});
