const khora = {
  name: 'Khora',
  scopeName: 'source.khora',
  fileTypes: ['kh'],
  patterns: [
    { include: '#comments' },
    { include: '#strings' },
    { include: '#numbers' },
    { include: '#keywords' },
    { include: '#types' },
    { include: '#functions' },
    { include: '#rowvars' },
    { include: '#operators' },
  ],
  repository: {
    comments: {
      patterns: [
        {
          name: 'comment.line.double-slash.khora',
          match: '//.*$',
        },
        {
          name: 'comment.block.khora',
          begin: '/\\*',
          end: '\\*/',
          patterns: [{ include: '#comments' }],
        },
      ],
    },
    strings: {
      patterns: [
        {
          name: 'string.quoted.double.khora',
          begin: '"',
          end: '"',
          patterns: [
            {
              name: 'constant.character.escape.khora',
              match: '\\\\.',
            },
            {
              name: 'meta.interpolation.khora',
              begin: '\\{',
              end: '\\}',
              patterns: [{ include: '$self' }],
            },
          ],
        },
      ],
    },
    numbers: {
      patterns: [
        {
          name: 'constant.numeric.float.khora',
          match: '\\b[0-9][0-9_]*\\.[0-9][0-9_]*(?:[eE][+-]?[0-9]+)?\\b',
        },
        {
          name: 'constant.numeric.integer.khora',
          match: '\\b[0-9][0-9_]*\\b',
        },
        {
          name: 'constant.language.boolean.khora',
          match: '\\b(?:true|false)\\b',
        },
      ],
    },
    keywords: {
      patterns: [
        {
          name: 'keyword.control.khora',
          match: '\\b(?:if|else|match|while|loop|for|break|continue|return|raise|catch)\\b',
        },
        {
          name: 'keyword.declaration.khora',
          match: '\\b(?:module|import|type|trait|impl|effect|context|fn|let|test|bench|export|const)\\b',
        },
        {
          name: 'storage.modifier.khora',
          match: '\\b(?:mut|forall|as|with|raises|handler)\\b',
        },
      ],
    },
    types: {
      patterns: [
        {
          name: 'support.type.builtin.khora',
          match: '\\b(?:Int|Float|Bool|String|Never|Option|Result|List)\\b',
        },
        {
          name: 'entity.name.type.khora',
          match: '\\b[A-Z][A-Za-z0-9_]*\\b',
        },
      ],
    },
    functions: {
      patterns: [
        {
          name: 'entity.name.function.khora',
          match: '(?<=\\bfn\\s)[A-Za-z_][A-Za-z0-9_]*',
        },
        {
          name: 'entity.name.namespace.khora',
          match: '\\b[A-Za-z_][A-Za-z0-9_]*(?=::)',
        },
      ],
    },
    rowvars: {
      patterns: [
        {
          name: 'variable.other.row.khora',
          match: "'[A-Za-z_][A-Za-z0-9_]*",
        },
      ],
    },
    operators: {
      patterns: [
        {
          // Before the pipe and before logical-or: `||>` starts with both, and
          // the first pattern that matches wins rather than the longest.
          name: 'keyword.operator.flow.khora',
          match: '\\|\\|>',
        },
        {
          name: 'keyword.operator.pipe.khora',
          match: '\\|>',
        },
        {
          name: 'keyword.operator.path.khora',
          match: '::',
        },
        {
          name: 'keyword.operator.khora',
          match: '=>|->|==|!=|<=|>=|&&|\\|\\||[+\\-*/%=<>!]',
        },
      ],
    },
  },
};

export default khora;
