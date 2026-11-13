#!/usr/bin/env python3
"""Reports a test helper compiled on a configuration where nothing calls it.

    python3 scripts/cfg-callers.py                  # every crates/*/tests/*.rs
    python3 scripts/cfg-callers.py FILE...          # these files only
    python3 scripts/cfg-callers.py --self-test      # its own cases

**What this prevents: a helper dead on one platform, found by that platform's
CI.** A helper in `crates/*/tests/*.rs` whose `#[cfg(..)]` is wider than the
union of its callers' is compiled where no caller is, which is a dead-code
warning there and, under `clippy -D warnings`, a red build on the one runner
nobody looked at locally. It happened twice: `targets.rs::linkage` was
`cfg(target_os = "linux")` while its callers were gated on x86-64 or musl,
so arm64 glibc Linux compiled it with nothing calling it; and
`permissions.rs::LISTENS` had no cfg while its only caller was `llvm`-only.

The rule: a helper's cfg must imply the `any(..)` of its callers' cfgs. Each
cfg is evaluated over every combination of operating system, architecture,
environment, vendor, pointer width and the features and flags the file
names, so `all(target_os = "linux", target_arch = "x86_64")` implies
`target_os = "linux"` and not the other way round, and `target_os = "macos"`
implies `target_vendor = "apple"`. A `#[cfg]` inside a caller's body,
including one on the same line as the statement, narrows the use under it.
A helper reported although it carries `#[allow(dead_code)]` is still
compiled where nothing uses it.

**What it costs and what it does not see.** A text scan, not a compiler:
- items at the top level of `crates/*/tests/*.rs` only, and uses by name; a
  shared helper module under `tests/<dir>/` is not read, and a local binding
  that shares a helper's name counts as a use of it;
- a `pub` helper's callers are looked for in the files of the same test
  crate (a root file and the modules it declares with `mod x;`);
- these are a *warning*, printed, not an error, and do not fail the run:
  a cfg it cannot read (`target_has_atomic`, a macro in a cfg, anything but
  `all`/`any`/`not`, `key = "value"` and bare flags), on the helper or on an
  item that uses it; and a helper named inside a `macro_rules!`, because
  where a macro expands is where it is invoked, which a text scan does not
  follow.

`--self-test` runs the shapes above, and the ones a review found, against
what each must report.

Exit status 1 when anything is reported as an error, 0 otherwise.
"""
import itertools
import os
import re
import sys

# --- lexing --------------------------------------------------------------


def blank(text):
    """`text` with comments and the insides of string and char literals
    replaced by spaces, newlines kept, so a name in a Khora program embedded
    in a test, or in a doc comment, is not taken for a use."""
    out = []
    i, n = 0, len(text)

    def keep_newlines(s):
        return ''.join('\n' if c == '\n' else ' ' for c in s)

    while i < n:
        c = text[i]
        if text.startswith('//', i):
            j = text.find('\n', i)
            j = n if j < 0 else j
            out.append(' ' * (j - i))
            i = j
        elif text.startswith('/*', i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith('/*', j):
                    depth, j = depth + 1, j + 2
                elif text.startswith('*/', j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(keep_newlines(text[i:j]))
            i = j
        elif (c == 'r' or text.startswith('br', i)) and re.match(r'b?r#*"', text[i:i + 300]) \
                and not (i > 0 and (text[i - 1].isalnum() or text[i - 1] == '_')):
            m = re.match(r'b?r(#*)"', text[i:])
            assert m is not None  # matched just above
            close = '"' + m.group(1)
            start = i + m.end()
            j = text.find(close, start)
            j = n if j < 0 else j + len(close)
            out.append('"' + keep_newlines(text[i + 1:j - 1]) + '"')
            i = j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == '\\' else 1
            j = min(j + 1, n)
            out.append('"' + keep_newlines(text[i + 1:j - 1]) + '"')
            i = j
        elif c == "'":
            m = re.match(r"'(\\.[^']*|[^'\\\n])'", text[i:])
            if m:
                out.append("'" + ' ' * (m.end() - 2) + "'")
                i += m.end()
            else:
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return ''.join(out)


# --- cfg expressions -----------------------------------------------------


class Unreadable(Exception):
    pass


def parse_cfg(src):
    """A cfg predicate as a tree: ('all', [..]), ('any', [..]), ('not', x),
    ('kv', key, value) or ('flag', name)."""
    toks = re.findall(r'"[^"]*"|[A-Za-z_][A-Za-z0-9_]*|[(),=]|\S', src)
    if any(not re.match(r'"|[A-Za-z_]|[(),=]$', t) for t in toks):
        raise Unreadable(src)
    pos = 0

    def expr():
        nonlocal pos
        if pos >= len(toks):
            raise Unreadable(src)
        t = toks[pos]
        pos += 1
        if t in ('all', 'any', 'not') and pos < len(toks) and toks[pos] == '(':
            pos += 1
            args = []
            while toks[pos] != ')':
                args.append(expr())
                if toks[pos] == ',':
                    pos += 1
            pos += 1
            if t == 'not':
                if len(args) != 1:
                    raise Unreadable(src)
                return ('not', args[0])
            return (t, args)
        if not re.match(r'[A-Za-z_]', t):
            raise Unreadable(src)
        if pos < len(toks) and toks[pos] == '=':
            pos += 1
            v = toks[pos]
            pos += 1
            if not v.startswith('"'):
                raise Unreadable(src)
            return ('kv', t, v.strip('"'))
        return ('flag', t)

    tree = expr()
    if pos != len(toks):
        raise Unreadable(src)
    return tree


TRUE = ('all', [])

# The platforms a cfg is judged over. Each is a consistent set of facts; a
# predicate about something not listed (a feature, `debug_assertions`) is a
# free variable and every value is tried.
PLATFORMS = []
for os_, family, vendor, envs in [
    ('linux', 'unix', 'unknown', ['gnu', 'musl']),
    ('macos', 'unix', 'apple', ['']),
    ('ios', 'unix', 'apple', ['']),
    ('freebsd', 'unix', 'unknown', ['']),
    # A unix the tests never name, so `cfg(unix)` is not taken to mean
    # "Linux, macOS or FreeBSD" because those are all that is listed.
    ('netbsd', 'unix', 'unknown', ['']),
    ('windows', 'windows', 'pc', ['msvc', 'gnu']),
]:
    for arch in ['x86_64', 'aarch64', 'x86', 'arm']:
        width = '32' if arch in ('x86', 'arm') else '64'
        for env in envs:
            PLATFORMS.append({
                'target_os': os_, 'target_family': family, 'target_arch': arch,
                'target_env': env, 'target_pointer_width': width, 'target_vendor': vendor,
                'unix': family == 'unix', 'windows': family == 'windows',
            })
PLATFORM_KEYS = {'target_os', 'target_family', 'target_arch', 'target_env', 'target_pointer_width',
                 'target_vendor'}
PLATFORM_FLAGS = {'unix', 'windows'}


def free_atoms(tree, acc):
    kind = tree[0]
    if kind in ('all', 'any'):
        for t in tree[1]:
            free_atoms(t, acc)
    elif kind == 'not':
        free_atoms(tree[1], acc)
    elif kind == 'kv' and tree[1] not in PLATFORM_KEYS:
        acc.add(('kv', tree[1], tree[2]))
    elif kind == 'flag' and tree[1] not in PLATFORM_FLAGS:
        acc.add(('flag', tree[1]))
    return acc


def holds(tree, platform, free):
    kind = tree[0]
    if kind == 'all':
        return all(holds(t, platform, free) for t in tree[1])
    if kind == 'any':
        return any(holds(t, platform, free) for t in tree[1])
    if kind == 'not':
        return not holds(tree[1], platform, free)
    if kind == 'kv':
        if tree[1] in PLATFORM_KEYS:
            return platform[tree[1]] == tree[2]
        return free[tree]
    if tree[1] in PLATFORM_FLAGS:
        return platform[tree[1]]
    return free[tree]


def counterexample(helper, callers):
    """A configuration where `helper` holds and no caller's cfg does, or
    None when the helper's cfg implies their union."""
    union = ('any', callers)
    atoms = sorted(free_atoms(helper, free_atoms(union, set())))
    for platform in PLATFORMS:
        for values in itertools.product([False, True], repeat=len(atoms)):
            free = dict(zip(atoms, values))
            if holds(helper, platform, free) and not holds(union, platform, free):
                facts = [f"target_os={platform['target_os']}", f"target_arch={platform['target_arch']}"]
                if platform['target_env']:
                    facts.append(f"target_env={platform['target_env']}")
                for atom, v in free.items():
                    name = atom[1] if atom[0] == 'flag' else f'{atom[1]}="{atom[2]}"'
                    facts.append(name if v else f'not {name}')
                return ', '.join(facts)
    return None


def show(tree):
    kind = tree[0]
    if kind in ('all', 'any'):
        if kind == 'all' and not tree[1]:
            return 'no cfg'
        return f"{kind}({', '.join(show(t) for t in tree[1])})"
    if kind == 'not':
        return f'not({show(tree[1])})'
    if kind == 'kv':
        return f'{tree[1]} = "{tree[2]}"'
    return tree[1]


def conj(trees):
    trees = [t for t in trees if t != TRUE]
    if not trees:
        return TRUE
    return trees[0] if len(trees) == 1 else ('all', trees)


# --- items ---------------------------------------------------------------

ITEM = re.compile(
    r'^(?:pub(?:\([\w:]+\))?\s+)?(?:(?:async|unsafe|const|extern(?:\s+"\w+")?)\s+)*'
    r'(fn|const|static|struct|enum|type|trait|mod|impl|union)\b\s*(?:<[^>]*>\s*)?([A-Za-z_]\w*)?'
)


class Item:
    def __init__(self, kind, name, line, end, cfg, cfg_text, is_test, public, body_start):
        self.kind, self.name, self.line, self.end = kind, name, line, end
        self.cfg, self.cfg_text, self.is_test, self.public = cfg, cfg_text, is_test, public
        self.body_start = body_start
        self.text = ''


def attributes_above(lines, i):
    """The attributes directly above line `i`: the run of attribute, doc
    and comment lines up to the previous blank line or item, with each
    `#[..]` taken whole however many lines it spans."""
    j = i - 1
    while j >= 0:
        s = lines[j].strip()
        if s == '' or s.endswith(('}', ';', '{')) and not s.startswith(('#', '//')):
            break
        j -= 1
    block = '\n'.join(l for l in lines[j + 1:i] if not l.strip().startswith('//'))
    attrs, k = [], 0
    while True:
        k = block.find('#[', k)
        if k < 0:
            break
        depth, e = 0, k + 1
        while e < len(block):
            if block[e] == '[':
                depth += 1
            elif block[e] == ']':
                depth -= 1
                if depth == 0:
                    break
            e += 1
        attrs.append(re.sub(r'\s+', ' ', block[k:e + 1]))
        k = e + 1
    return attrs, j + 1


def cfg_of(attrs, warn):
    found, texts = [], []
    for a in attrs:
        m = re.match(r'#\[\s*cfg\s*\((.*)\)\s*\]$', a, re.S)
        if not m:
            continue
        texts.append(m.group(1).strip())
        try:
            found.append(parse_cfg(m.group(1)))
        except Unreadable:
            warn(f'cannot read cfg({m.group(1).strip()})')
            return None, ' && '.join(texts)
    return conj(found), ' && '.join(texts)


def items_of(text, blanked, warn_at):
    lines = text.split('\n')
    blines = blanked.split('\n')
    # `#![cfg(..)]` at the top of a file gates everything in it.
    file_cfgs = []
    for m in re.finditer(r'^#!\[\s*cfg\s*\((.*)\)\s*\]\s*$', text, re.M):
        try:
            file_cfgs.append(parse_cfg(m.group(1)))
        except Unreadable:
            warn_at(text[:m.start()].count('\n') + 1, f'cannot read cfg({m.group(1).strip()})')
            return []
    out = []
    i = 0
    while i < len(blines):
        line = blines[i]
        m = ITEM.match(line) if line and not line[0].isspace() else None
        if not m:
            i += 1
            continue
        kind, name = m.group(1), m.group(2)
        attrs, _ = attributes_above(lines, i)
        cfg, cfg_text = cfg_of(attrs, lambda msg, at=i, n=name: warn_at(at + 1, f'`{n}`: {msg}'))
        if cfg is not None:
            cfg = conj(file_cfgs + [cfg])
        is_test = any(re.match(r'#\[\s*(test|tokio::test|bench)\b', a) for a in attrs)
        depth, started, k = 0, False, i
        while k < len(blines):
            for ch in blines[k]:
                if ch == '{':
                    depth += 1
                    started = True
                elif ch == '}':
                    depth -= 1
            if started and depth <= 0:
                break
            if not started and blines[k].rstrip().endswith(';') and depth == 0:
                break
            k += 1
        if kind == 'mod' and blines[i].rstrip().endswith(';'):
            i = k + 1
            continue
        out.append(Item(kind, name or '', i, min(k, len(blines) - 1), cfg, cfg_text, is_test,
                        line.startswith('pub'), i))
        out[-1].text = '\n'.join(blines[i:k + 1])
        i = k + 1
    return out


def macro_bodies(blanked):
    """Each `macro_rules!` in a file: its name and its body, blanked."""
    out = []
    for m in re.finditer(r'^[ \t]*(?:#\[[^\]]*\]\s*)*macro_rules!\s*(\w+)\s*\{', blanked, re.M):
        depth, e = 1, m.end()
        while e < len(blanked) and depth:
            if blanked[e] == '{':
                depth += 1
            elif blanked[e] == '}':
                depth -= 1
            e += 1
        out.append((m.group(1), blanked[m.end():e]))
    return out


def leading_cfg(raw):
    """`(predicate, rest)` for a line that starts with a `#[cfg(..)]` and
    has a statement after it on the same line, or None."""
    m = re.match(r'#\[\s*cfg\s*\(', raw)
    if not m:
        return None
    depth, e = 1, m.end()
    while e < len(raw) and depth:
        if raw[e] == '(':
            depth += 1
        elif raw[e] == ')':
            depth -= 1
        e += 1
    close = re.match(r'\s*\]\s*', raw[e:])
    if depth or not close:
        return None
    rest = raw[e + close.end():]
    return (raw[m.end():e - 1], rest) if rest.strip() else None


def uses_in(item, blines, name, raw_lines, warn_at):
    """The cfg under which each use of `name` in `item`'s body sits: the
    item's own cfg, narrowed by any `#[cfg]` attribute inside the body that
    governs the statement or block the use is in."""
    pat = re.compile(r'(?<![\w.])' + re.escape(name) + r'\b')
    found = []
    stack = []  # (depth at which the attribute's region ends, cfg)
    depth = 0
    pending = None
    for ln in range(item.line, item.end + 1):
        b = blines[ln]
        raw = raw_lines[ln].strip()
        inner = re.match(r'#\[\s*cfg\s*\((.*)\)\s*\]\s*$', raw)
        if ln > item.line and inner:
            try:
                pending = parse_cfg(inner.group(1))
            except Unreadable:
                warn_at(ln + 1, f'cannot read cfg({inner.group(1).strip()})')
                pending = None
            continue
        same_line = leading_cfg(raw) if ln > item.line else None
        if same_line:
            # `#[cfg(..)] let _ = helper();` -- the attribute and the
            # statement it governs on one line.
            try:
                pending = parse_cfg(same_line[0])
            except Unreadable:
                warn_at(ln + 1, f'cannot read cfg({same_line[0].strip()})')
                pending = None
        if ln == item.line and item.kind in ('fn', 'impl', 'mod', 'trait'):
            # The item's own name on its first line is its declaration.
            sig = b
        else:
            sig = None
        if pending is not None:
            stack.append((depth, pending, ln))
            pending = None
        region = conj([item.cfg or TRUE] + [c for _, c, _ in stack])
        if (sig is None or ln != item.line or item.name != name) and pat.search(b):
            found.append((ln, region))
        for ch in b:
            if ch == '{':
                depth += 1
            elif ch == '}':
                depth -= 1
        # A statement's attribute governs that statement: until the block it
        # opened closes, or the line ends it when it opened none.
        while stack and (depth <= stack[-1][0]) and (
                stack[-1][2] != ln or b.rstrip().endswith((';', '}', ','))):
            stack.pop()
    return found


# --- crates --------------------------------------------------------------


def crates_of(paths):
    """Groups test files into crates: a file declared as `mod x;` by another
    file in the same directory belongs to that file's crate."""
    by_dir = {}
    for p in paths:
        by_dir.setdefault(os.path.dirname(p), []).append(p)
    groups = []
    for d, files in by_dir.items():
        stems = {os.path.splitext(os.path.basename(f))[0]: f for f in files}
        parent = {}
        for f in files:
            try:
                src = open(f, encoding='utf-8').read()
            except OSError:
                continue
            for m in re.finditer(r'^\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?mod\s+(\w+)\s*;', src, re.M):
                if m.group(1) in stems and stems[m.group(1)] != f:
                    parent[stems[m.group(1)]] = f
        root_of = {}
        for f in files:
            r, seen = f, set()
            while r in parent and r not in seen:
                seen.add(r)
                r = parent[r]
            root_of[f] = r
        crates = {}
        for f in files:
            crates.setdefault(root_of[f], []).append(f)
        groups.extend(crates.values())
    return groups


def main(argv):
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    if argv:
        paths = argv
    else:
        paths = []
        crates_dir = os.path.join(root, 'crates')
        for c in sorted(os.listdir(crates_dir)):
            t = os.path.join(crates_dir, c, 'tests')
            if os.path.isdir(t):
                paths += [os.path.join(t, f) for f in sorted(os.listdir(t)) if f.endswith('.rs')]

    errors = warnings = 0
    for crate in crates_of(paths):
        parsed = {}
        macros = {}
        for path in crate:
            text = open(path, encoding='utf-8').read()
            blanked = blank(text)
            macros[path] = macro_bodies(blanked)
            notes = []

            def warn_at(line, msg, path=path, notes=notes):
                notes.append((line, msg))
            its = items_of(text, blanked, warn_at)
            parsed[path] = (text.split('\n'), blanked.split('\n'), its, notes)

        for path in crate:
            raw_lines, blines, its, notes = parsed[path]
            rel = os.path.relpath(path, root) if path.startswith(root) else path
            for line, msg in notes:
                print(f'{rel}:{line}: warning: {msg}')
                warnings += 1
            for helper in its:
                if helper.is_test or not helper.name or helper.kind in ('impl', 'mod') or helper.name == 'main':
                    continue
                if helper.cfg is None:
                    continue  # its own cfg is unreadable, and already said so
                search = crate if helper.public else [path]
                uses = []
                unreadable = False
                in_macro = []
                pat = re.compile(r'(?<![\w.])' + re.escape(helper.name) + r'\b')
                for other_path in search:
                    o_raw, o_blines, o_items, _ = parsed[other_path]
                    for macro, body in macros[other_path]:
                        if pat.search(body):
                            in_macro.append(macro)
                    for other in o_items:
                        if other is helper:
                            continue
                        if other.kind == 'impl' and other.name == helper.name and helper.kind in ('struct', 'enum', 'type', 'union'):
                            continue  # its own impls are not callers
                        if helper.name not in other.text:
                            continue
                        found = uses_in(other, o_blines, helper.name, o_raw, lambda *a: None)
                        if not found:
                            continue
                        if other.cfg is None:
                            unreadable = True
                            continue
                        uses += [c for _, c in found]
                if in_macro:
                    # Where a macro expands is where its invocations are,
                    # which a text scan of the macro does not know.
                    print(f'{rel}:{helper.line + 1}: warning: `{helper.name}` is used through '
                          f'`{in_macro[0]}!`, whose expansions this cannot see; not judged')
                    warnings += 1
                    continue
                if not uses and not unreadable:
                    continue  # unused anywhere: the compiler says so itself
                if unreadable:
                    print(f'{rel}:{helper.line + 1}: warning: `{helper.name}` has a caller with a cfg this cannot read; not judged')
                    warnings += 1
                    continue
                if any(u == TRUE for u in uses):
                    continue
                distinct = []
                for u in uses:
                    if u not in distinct:
                        distinct.append(u)
                where = counterexample(helper.cfg, distinct)
                if where:
                    callers = show(distinct[0]) if len(distinct) == 1 else show(('any', distinct))
                    print(f'{rel}:{helper.line + 1}: error: {helper.kind} `{helper.name}` is compiled under '
                          f'{show(helper.cfg)} but used only under {callers}; nothing uses it on {where}')
                    errors += 1
    if errors or warnings:
        print(f'cfg-callers: {errors} error(s), {warnings} warning(s)', file=sys.stderr)
    return 1 if errors else 0


# --- self-test -----------------------------------------------------------

# Each case is one test file and what must be reported in it: the helpers
# reported as errors, and those reported as warnings. A shape this misses on
# purpose is here too, with what it reports, so a change to it is a decision
# and not an accident.
SELF_TEST = {
    'too_wide': ('''
#[cfg(unix)]
fn helper() -> u32 { 3 }

#[cfg(target_os = "linux")]
#[test]
fn uses_it() { assert_eq!(helper(), 3); }
''', {'helper'}, set()),
    'exact': ('''
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn exact() {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a() { exact(); }
''', set(), set()),
    'covered_by_two': ('''
#[cfg(unix)]
fn helper() -> u32 { 3 }

#[cfg(target_os = "linux")]
#[test]
fn on_linux() { assert_eq!(helper(), 3); }

#[cfg(all(unix, not(target_os = "linux")))]
#[test]
fn on_other_unix() { assert_eq!(helper(), 3); }

#[cfg(feature = "llvm")]
fn two() -> u32 { 2 }

#[cfg(all(feature = "llvm", unix))]
#[test]
fn two_unix() { assert_eq!(two(), 2); }

#[cfg(all(feature = "llvm", not(unix)))]
#[test]
fn two_not_unix() { assert_eq!(two(), 2); }
''', set(), set()),
    # `target_vendor = "apple"` holds on macOS, so a macOS helper with an
    # Apple caller is fine; the other way round is dead on iOS.
    'vendor': ('''
#[cfg(target_os = "macos")]
fn mac() -> u32 { 1 }

#[cfg(target_vendor = "apple")]
#[test]
fn on_apple() { assert_eq!(mac(), 1); }

#[cfg(target_vendor = "apple")]
fn apple() -> u32 { 1 }

#[cfg(target_os = "macos")]
#[test]
fn on_mac() { assert_eq!(apple(), 1); }
''', {'apple'}, set()),
    'ungated_const_gated_caller': ('''
const LISTENS: &str = "x";

#[cfg(feature = "llvm")]
#[test]
fn b() { let _ = LISTENS; }
''', {'LISTENS'}, set()),
    'statement_cfg': ('''
fn inner_only() {}

#[test]
fn f() {
    #[cfg(windows)]
    inner_only();
}

fn same_line() -> u32 { 1 }

#[test]
fn g() {
    #[cfg(target_os = "linux")] let _ = same_line();
}
''', {'inner_only', 'same_line'}, set()),
    'in_a_string': ('''
fn named_in_text() {}

#[cfg(unix)]
#[test]
fn t() { let s = r#"named_in_text()"#; let _ = s; }
''', set(), set()),
    'through_a_macro': ('''
fn helper() -> u32 { 3 }

macro_rules! check {
    () => { assert_eq!(helper(), 3) };
}

#[test]
fn everywhere() { check!(); }

#[cfg(target_os = "linux")]
#[test]
fn on_linux() { assert_eq!(helper(), 3); }

fn only_in_macro() -> u32 { 2 }

macro_rules! use_it {
    () => { only_in_macro() };
}

#[cfg(target_os = "linux")]
#[test]
fn macro_user() { assert_eq!(use_it!(), 2); }
''', set(), {'helper', 'only_in_macro'}),
    'gated_inline_mod': ('''
fn d3() -> u32 { 3 }

#[cfg(windows)]
mod win {
    #[test]
    fn d3_user() { assert_eq!(super::d3(), 3); }
}
''', {'d3'}, set()),
    'unrelated_odd_cfg': ('''
#[cfg(feature = "a b")]
fn unrelated() {}

#[cfg(unix)]
fn d4() -> u32 { 4 }

#[cfg(target_os = "linux")]
#[test]
fn d4_user() { assert_eq!(d4(), 4); }
''', {'d4'}, set()),
    'unreadable': ('''
#[cfg(some_macro!())]
fn odd() {}

#[cfg(unix)]
#[test]
fn a() { odd(); }
''', set(), {'odd'}),
    'chained': ('''
fn d5a() -> u32 { 5 }

fn d5b() -> u32 { d5a() }

#[cfg(feature = "llvm")]
#[test]
fn d5_user() { assert_eq!(d5b(), 5); }
''', {'d5b'}, set()),
    'multi_line_attribute': ('''
fn helper() -> u32 { 3 }

#[cfg(all(
    feature = "llvm",
    target_os = "linux"
))]
#[test]
fn uses_it() { assert_eq!(helper(), 3); }
''', {'helper'}, set()),
    # Known miss: a local binding with the helper's name counts as a use.
    'shadowed_by_a_local': ('''
fn d6() -> u32 { 6 }

#[test]
fn d6_shadow() { let d6 = 7; assert_eq!(d6, 7); }

#[cfg(target_os = "linux")]
#[test]
fn d6_user() { assert_eq!(d6(), 6); }
''', set(), set()),
    # Reported although `allow(dead_code)` keeps the compiler quiet: the
    # helper is still compiled where nothing uses it.
    'allowed_dead': ('''
#[cfg(unix)]
#[allow(dead_code)]
fn helper() -> u32 { 3 }

#[cfg(target_os = "linux")]
#[test]
fn uses_it() { assert_eq!(helper(), 3); }
''', {'helper'}, set()),
}


def self_test():
    """Runs every case in [`SELF_TEST`] and says which disagree."""
    import io
    import contextlib
    import tempfile
    failed = 0
    with tempfile.TemporaryDirectory() as tmp:
        for name, (src, want_errors, want_warnings) in SELF_TEST.items():
            d = os.path.join(tmp, name)
            os.makedirs(d)
            path = os.path.join(d, f'{name}.rs')
            with open(path, 'w', encoding='utf-8') as f:
                f.write(src)
            out = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                code = main([path])
            got_errors, got_warnings = set(), set()
            for line in out.getvalue().splitlines():
                m = re.search(r': (error|warning): .*?`(\w+)`', line)
                if m:
                    (got_errors if m.group(1) == 'error' else got_warnings).add(m.group(2))
            want_code = 1 if want_errors else 0
            if (got_errors, got_warnings, code) != (want_errors, want_warnings, want_code):
                failed += 1
                print(f'self-test {name}: errors {sorted(got_errors)} (want {sorted(want_errors)}), '
                      f'warnings {sorted(got_warnings)} (want {sorted(want_warnings)}), '
                      f'exit {code} (want {want_code})')
                for line in out.getvalue().splitlines():
                    print(f'    {line}')
    print(f'cfg-callers self-test: {len(SELF_TEST) - failed} of {len(SELF_TEST)} cases agree')
    return 1 if failed else 0


if __name__ == '__main__':
    if sys.argv[1:] == ['--self-test']:
        sys.exit(self_test())
    sys.exit(main(sys.argv[1:]))
