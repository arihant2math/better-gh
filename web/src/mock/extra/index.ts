/**
 * Mock handlers for account, settings, profile and repo-settings endpoints
 * (package account-web). Each area registers its routes on the server;
 * non-synced state lives in a per-server object (`state(server)`), synced
 * models go through `server.put` / `server.remove` like the real backend.
 */
import type { MockServer } from '../server';
import { installAuthMocks } from './auth';
import { installDeveloperMocks } from './developer';
import { installProfileMocks } from './profile';
import { installRepoSettingsMocks } from './repo';
import { installUserMocks } from './user';

export function installExtraMocks(server: MockServer): void {
  installAuthMocks(server);
  installUserMocks(server);
  installDeveloperMocks(server);
  installProfileMocks(server);
  installRepoSettingsMocks(server);
}
