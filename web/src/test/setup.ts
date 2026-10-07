import 'fake-indexeddb/auto';
import { configure } from 'mobx';

configure({ enforceActions: 'always' });

// Mock feature chunks load in `src/test/mockServer.ts`, only for tests that use the mock.
