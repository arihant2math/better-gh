// The local observer rule (#203): store reads in a component need observer().
import { RuleTester } from "eslint";
import tseslint from "typescript-eslint";
import { it } from "vitest";
import bgh from "./eslint-rules.mjs";

const tester = new RuleTester({
  languageOptions: {
    parser: tseslint.parser,
    parserOptions: { ecmaFeatures: { jsx: true } },
  },
});
const head =
  "import { observer } from 'mobx-react-lite';\nimport { store } from '../sync';\nimport { repoByName } from '../sync/selectors';\n";

it("observer-reads-store", () =>
  tester.run("observer-reads-store", bgh.rules["observer-reads-store"], {
    valid: [
      head +
        "export default observer(function Page() { return <p>{repoByName('a', 'b')?.name}</p>; });",
      head +
        "export const Row = observer(({ id }: { id: number }) => <p>{store().get('user', id)?.login}</p>);",
      head +
        "const List = observer(function List() { return <>{[1].map((id) => store().get('user', id)?.login)}</>; });",
      // Reads outside render (handlers, effects) and in hooks are fine.
      head +
        "function Button() { return <button onClick={() => store().all('user')}>x</button>; }",
      head +
        "function Effect() { useEffect(() => { store().all('user'); }, []); return null; }",
      head + "export function useRepo() { return repoByName('a', 'b'); }",
      // Type-only and non-read imports don't count.
      "import { hasSync } from '../sync';\nfunction Gate() { return hasSync() ? <p /> : null; }",
    ],
    invalid: [
      {
        code:
          head +
          "export default function Page() { return <p>{repoByName('a', 'b')?.name}</p>; }",
        errors: [
          { messageId: "missing", data: { name: "Page", read: "repoByName" } },
        ],
      },
      {
        code:
          head +
          "const Row = ({ id }: { id: number }) => <p>{store().get('user', id)?.login}</p>;",
        errors: [
          { messageId: "missing", data: { name: "Row", read: "store" } },
        ],
      },
      {
        code:
          head +
          "const List = memo(function List() { return <>{[1].map((id) => store().get('user', id)?.login)}</>; });",
        errors: [
          { messageId: "missing", data: { name: "List", read: "store" } },
        ],
      },
      {
        code:
          head +
          "function Search({ q }: { q: string }) { const hits = useMemo(() => store().all('user'), [q]); return <p>{hits.length}</p>; }",
        errors: [
          { messageId: "missing", data: { name: "Search", read: "store" } },
        ],
      },
    ],
  }));
