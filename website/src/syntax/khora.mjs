import { readFileSync } from 'node:fs';

// Keep documentation highlighting on the same grammar as the editor extension.
// Shiki uses `name` as the fenced-code language id, while the TextMate grammar's
// display name is "Khora". Register the lowercase id used by Markdown fences and
// retain `kh` as the source-file alias.
const grammarPath = new URL('../../../editors/vscode/syntaxes/khora.tmLanguage.json', import.meta.url);
const grammar = JSON.parse(readFileSync(grammarPath, 'utf8'));

const khora = {
  ...grammar,
  name: 'khora',
  displayName: grammar.name,
  aliases: ['kh'],
};

export default khora;
