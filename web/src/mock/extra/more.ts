/**
 * Second half of the extra mocks (split from `./index` to keep each mock
 * chunk under the lazy-chunk budget). Installed right after
 * `installExtraMocks`, so `installLifecycleMocks` still runs last.
 */
import type { MockServer } from '../server';
import { installCacheMocks } from '../caches';
import { installFineGrainedTokenMocks } from './fineGrainedTokens';
import { installLifecycleMocks } from './lifecycle';
import { installModerationMocks } from './moderation';
import { installRelationshipMocks } from './relationships';
import { installRunnerMocks } from './runners';
import { installSamlMocks } from './saml';
import { installSecretScanningMocks } from './secretScanning';

export function installMoreExtraMocks(server: MockServer): void {
  installRelationshipMocks(server);
  installModerationMocks(server);
  installRunnerMocks(server);
  installSamlMocks(server);
  installCacheMocks(server);
  installFineGrainedTokenMocks(server);
  installSecretScanningMocks(server);
  // Last: its overrides (rename, transfer to a user, delete snapshots) run before the routes above.
  installLifecycleMocks(server);
}
