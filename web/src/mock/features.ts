/**
 * Feature areas of the mock backend, each its own lazy chunk (keeps every
 * mock chunk under the lazy-chunk budget). `MockServer.create` loads them
 * before constructing a server; tests load them via `src/test/mockServer.ts`.
 */
import type { installActionsRoutes } from './actions';
import type { installCodeRoutes } from './code';
import type { installExtraMocks } from './extra';
import type { installMoreExtraMocks } from './extra/more';
import type { installInboxSearchRoutes } from './inboxSearch';
import type { installProjectRoutes } from './projects';
import type { branchNames, pullDiffText, registerPullRoutes } from './pulls';
import type { installWikiRoutes } from './wiki';

export interface MockFeatures {
  installActionsRoutes: typeof installActionsRoutes;
  installCodeRoutes: typeof installCodeRoutes;
  installExtraMocks: typeof installExtraMocks;
  installMoreExtraMocks: typeof installMoreExtraMocks;
  installInboxSearchRoutes: typeof installInboxSearchRoutes;
  installProjectRoutes: typeof installProjectRoutes;
  installWikiRoutes: typeof installWikiRoutes;
  registerPullRoutes: typeof registerPullRoutes;
  branchNames: typeof branchNames;
  pullDiffText: typeof pullDiffText;
}

let loaded: MockFeatures | null = null;
let loading: Promise<MockFeatures> | null = null;

/** The loaded feature modules (throws before `loadMockFeatures()` resolved). */
export function mockFeatures(): MockFeatures {
  if (!loaded) throw new Error('mock features not loaded: await loadMockFeatures() first');
  return loaded;
}

export function loadMockFeatures(): Promise<MockFeatures> {
  loading ??= Promise.all([
    import('./actions'),
    import('./code'),
    import('./extra'),
    import('./extra/more'),
    import('./inboxSearch'),
    import('./projects'),
    import('./pulls'),
    import('./wiki'),
  ]).then(([actions, code, extra, extraMore, inbox, projects, pulls, wiki]) => {
    loaded = {
      installActionsRoutes: actions.installActionsRoutes,
      installCodeRoutes: code.installCodeRoutes,
      installExtraMocks: extra.installExtraMocks,
      installMoreExtraMocks: extraMore.installMoreExtraMocks,
      installInboxSearchRoutes: inbox.installInboxSearchRoutes,
      installProjectRoutes: projects.installProjectRoutes,
      installWikiRoutes: wiki.installWikiRoutes,
      registerPullRoutes: pulls.registerPullRoutes,
      branchNames: pulls.branchNames,
      pullDiffText: pulls.pullDiffText,
    };
    return loaded;
  });
  return loading;
}
