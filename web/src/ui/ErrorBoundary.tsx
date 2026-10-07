import { Component, type ErrorInfo, type ReactNode } from 'react';
import { isChunkLoadError, reloadForChunkError } from '../router/chunkError';
import { Button } from './Button';
import { EmptyState } from './EmptyState';
import styles from './ErrorBoundary.module.css';
import { AlertIcon } from './icons';

interface Props {
  children: ReactNode;
  /** Shown in console logs to say which boundary caught the error. */
  name: string;
  /** Changing this (e.g. the URL) clears a caught error and renders `children` again. */
  resetKey?: unknown;
  /**
   * `page`: fills the viewport (root boundary). `content`: fills its
   * container (route boundary). `silent`: renders nothing (overlays).
   */
  variant?: 'page' | 'content' | 'silent';
}

interface State {
  error: unknown;
  resetKey: unknown;
}

const NONE = Symbol('no error');

/**
 * Catches render errors (including failed lazy chunks) below it so one
 * broken page or overlay doesn't unmount the whole app. Stale chunks after a
 * deploy trigger one automatic reload (see `reloadForChunkError`).
 */
export class ErrorBoundary extends Component<Props, State> {
  override state: State = { error: NONE, resetKey: this.props.resetKey };

  static getDerivedStateFromError(error: unknown): Partial<State> {
    return { error };
  }

  static getDerivedStateFromProps(props: Props, state: State): Partial<State> | null {
    if (Object.is(props.resetKey, state.resetKey)) return null;
    return { resetKey: props.resetKey, error: NONE };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo): void {
    console.error(`[${this.props.name}] render error`, error, info.componentStack);
    if (isChunkLoadError(error)) reloadForChunkError();
  }

  retry = () => this.setState({ error: NONE });

  override render() {
    const { error } = this.state;
    if (error === NONE) return this.props.children;
    const { variant = 'content' } = this.props;
    if (variant === 'silent') return null;
    const chunk = isChunkLoadError(error);
    const fallback = (
      <div role="alert">
        <EmptyState
          icon={AlertIcon}
          title={chunk ? 'This page failed to load' : 'Something went wrong'}
          action={
            <div className={styles.actions}>
              <Button variant="primary" size="lg" onClick={this.retry}>
                Retry
              </Button>
              <Button size="lg" onClick={() => window.location.reload()}>
                Reload
              </Button>
            </div>
          }
        >
          {chunk
            ? 'Better GitHub may have been updated, or the network dropped. Reloading fetches the latest version.'
            : 'An unexpected error stopped this view from rendering. Retry, or reload the page if it keeps happening.'}
        </EmptyState>
      </div>
    );
    return variant === 'page' ? <div className={styles.page}>{fallback}</div> : fallback;
  }
}
