/**
 * Linguist language colors (data, like GitHub's language bar). These are
 * identity colors of languages, not theme colors, so they are the same in
 * both themes; unknown languages get a stable hue.
 */
const COLORS: Record<string, string> = {
  Rust: '#dea584',
  TypeScript: '#3178c6',
  JavaScript: '#f1e05a',
  Python: '#3572A5',
  Go: '#00ADD8',
  Shell: '#89e051',
  Ruby: '#701516',
  Java: '#b07219',
  Kotlin: '#A97BFF',
  Swift: '#F05138',
  C: '#555555',
  'C++': '#f34b7d',
  'C#': '#178600',
  PHP: '#4F5D95',
  HTML: '#e34c26',
  CSS: '#563d7c',
  SCSS: '#c6538c',
  Vue: '#41b883',
  Svelte: '#ff3e00',
  Dockerfile: '#384d54',
  Makefile: '#427819',
  Nix: '#7e7eff',
  Lua: '#000080',
  Haskell: '#5e5086',
  Elixir: '#6e4a7e',
  Erlang: '#B83998',
  Scala: '#c22d40',
  Dart: '#00B4AB',
  Zig: '#ec915c',
  Markdown: '#083fa1',
  TOML: '#9c4221',
  YAML: '#cb171e',
  Jupyter: '#DA5B0B',
  'Jupyter Notebook': '#DA5B0B',
  PLpgSQL: '#336790',
  SQL: '#e38c00',
  Perl: '#0298c3',
  R: '#198CE7',
  Objective: '#438eff',
  Other: '#8b949e',
};

export function languageColor(lang: string): string {
  const c = COLORS[lang];
  if (c) return c;
  let h = 0;
  for (let i = 0; i < lang.length; i++) h = (h * 31 + lang.charCodeAt(i)) >>> 0;
  return `hsl(${h % 360} 55% 55%)`;
}
