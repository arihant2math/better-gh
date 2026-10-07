// Local lint rules for conventions in docs/FRONTEND.md (#203).

/** Module specifiers whose imports read the sync store. */
const STORE_MODULE = /(^|\/)sync(\/(selectors|pullSelectors))?$/;
/** Exports of the sync index that are not store reads. */
const NON_READS = new Set(['setSyncClient', 'hasSync']);
const isComponentName = (name) => typeof name === 'string' && /^[A-Z]/.test(name);

/** Name a function is bound to: `function Foo`, `const Foo = …`, `const Foo = memo(…)`. */
function componentName(fn) {
  if (fn.id) return fn.id.name;
  let node = fn.parent;
  while (node?.type === 'CallExpression') node = node.parent;
  return node?.type === 'VariableDeclarator' && node.id.type === 'Identifier' ? node.id.name : undefined;
}

/** True when `fn` is (an argument of) an `observer(…)` call. */
function isObserved(fn) {
  for (let node = fn.parent; node?.type === 'CallExpression'; node = node.parent) {
    const callee = node.callee;
    if (callee.type === 'Identifier' && callee.name === 'observer') return true;
  }
  return false;
}

/**
 * Nested functions that run during render, so their store reads are the
 * component's: array callbacks (`rows.map(…)`) and memo-style hooks. Event
 * handlers and effects run outside render and need no tracking.
 */
function runsDuringRender(fn) {
  const call = fn.parent;
  if (call?.type !== 'CallExpression' || !call.arguments.includes(fn)) return false;
  const callee = call.callee;
  if (callee.type === 'Identifier') return callee.name === 'useMemo' || callee.name === 'useComputed';
  return callee.type === 'MemberExpression';
}

/** @type {import('eslint').Rule.RuleModule} */
const observerReadsStore = {
  meta: {
    type: 'problem',
    docs: { description: 'Components that read the sync store must be wrapped in observer().' },
    schema: [],
    messages: {
      missing: '`{{name}}` reads the store via `{{read}}()` but is not wrapped in observer(); it will not re-render when that data changes.',
    },
  },
  create(context) {
    const reads = new Set();
    const stack = [];
    const enter = (fn) => {
      const name = componentName(fn);
      const parent = stack.at(-1);
      if (isComponentName(name)) {
        stack.push({ name, observed: isObserved(fn) });
      } else if (parent && runsDuringRender(fn)) {
        stack.push(parent);
      } else {
        stack.push(null);
      }
    };
    const exit = () => stack.pop();
    return {
      ImportDeclaration(node) {
        if (node.importKind === 'type' || !STORE_MODULE.test(node.source.value)) return;
        for (const s of node.specifiers) {
          if (s.type === 'ImportSpecifier' && s.importKind !== 'type' && !NON_READS.has(s.local.name)) reads.add(s.local.name);
        }
      },
      FunctionDeclaration: enter,
      FunctionExpression: enter,
      ArrowFunctionExpression: enter,
      'FunctionDeclaration:exit': exit,
      'FunctionExpression:exit': exit,
      'ArrowFunctionExpression:exit': exit,
      CallExpression(node) {
        const top = stack.at(-1);
        if (!top || top.observed || node.callee.type !== 'Identifier' || !reads.has(node.callee.name)) return;
        context.report({ node, messageId: 'missing', data: { name: top.name, read: node.callee.name } });
      },
    };
  },
};

export default { meta: { name: 'bgh' }, rules: { 'observer-reads-store': observerReadsStore } };
