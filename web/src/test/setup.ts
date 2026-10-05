import 'fake-indexeddb/auto';
import { configure } from 'mobx';

configure({ enforceActions: 'always' });

// Mock backend feature modules are lazy chunks in the app; load them once.
await (await import('../mock/features')).loadMockFeatures();
