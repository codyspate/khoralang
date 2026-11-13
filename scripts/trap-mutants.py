#!/usr/bin/env python3
"""mutants.py NAME -- apply mutant NAME to the tree, saving originals; `mutants.py restore` puts them back.

Each mutant disables one part of the containment fix, so a test that guards that
part goes red. Run one at a time: apply, rebuild (rt and/or compiler), run the
named test, restore, rebuild. Never while another suite runs.
"""
import os, shutil, sys

TREE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SAVE = os.path.join(os.environ.get("TMPDIR", "/tmp"), "khora-trap-mutant-save")

# name -> (file, old, new, tests that must go red, what to rebuild)
MUTANTS = {
    # The trap exit returns its tag without releasing the frame: what the
    # abandoned frames owned is leaked on every contained trap.
    "noleak": ("crates/khora-codegen-llvm/src/lower/failure.rs",
               "            None if !self.raises && self.tagged => {\n                self.unwind_to(0);",
               "            None if !self.raises && self.tagged => {\n                if which.get_zero_extended_constant() != Some(runtime::TRAPPED_WHICH) { self.unwind_to(0); }",
               "containment::contained_traps_leak_nothing", "compiler"),
    # Operands live across a bounds or UTF-8 check are not held for the trap exit.
    "nohold": ("crates/khora-codegen-llvm/src/lower/rc.rs",
               "        for (operand, value) in held {\n            self.hold_operand(*operand, *value);\n        }",
               "        let _ = held;",
               "containment::contained_traps_leak_nothing", "compiler"),
    # A trap in a finalizer is contained like any other.
    "noshield": ("crates/khora-rt/src/fibertrap.rs",
                 "fiber.contained().load(Ordering::Relaxed) > 0 && !fiber.is_shielded()",
                 "fiber.contained().load(Ordering::Relaxed) > 0",
                 "containment::what_contain_leaves_fatal_stays_fatal", "rt"),
    # The budget never runs out.
    "nobudget": ("crates/khora-rt/src/fibertrap.rs",
                 "    if spent.len() >= most {\n        return false;\n    }",
                 "    let _ = most;",
                 "containment::the_trap_budget_makes_the_next_trap_fatal", "rt"),
    # `contain`'s closure is checked like any other closure.
    "nocapture": ("crates/khora-types/src/check/calls.rs",
                  "                Some(Guarded::Contain) => Some(super::sharing::Crossing::Contain),\n",
                  "                Some(Guarded::Contain) => None,\n",
                  "containment::contain_refuses_what_a_trap_could_leave_half_written", "compiler"),
    # Each return of a tagged function builds and returns its own pair, so a
    # trap exit reaches an inlining caller as a phi of pairs.
    "noexit": ("crates/khora-codegen-llvm/src/lower/failure.rs",
               "        let exit = match self.plain_exit {\n            Some(exit) => exit,",
               "        if std::env::var_os(\"\").is_none() {\n            let shape = self.plain_pair_type();\n            let value = self.be.builder.build_insert_value(shape.get_undef(), which, 0, \"t.which\").expect(\"tag\");\n            let value = self.be.builder.build_insert_value(value, answer, 1, \"t.answer\").expect(\"answer\");\n            self.be.builder.build_return(Some(&value.into_struct_value())).expect(\"ret\");\n            return;\n        }\n        let exit = match self.plain_exit {\n            Some(exit) => exit,",
               "polls::a_checked_loop_tests_no_tag_on_its_hot_path", "compiler"),
    # A program may call `contain_with`, which moves a value in without the
    # capture rule.
    "nostdonly": ("crates/khora-types/src/check/calls.rs",
                  "        if self.in_std {\n            return;\n        }",
                  "        if self.in_std || !self.in_std {\n            return;\n        }",
                  "containment::a_program_cannot_reach_contain_without_its_rule", "compiler"),
    # A program's own bodiless `contain_run` is lowered as the primitive.
    # Two guards hold this (the checker's "no body" and the backend's
    # `std::core` test), so each alone stays green: run `ownrun+ownrun2`.
    "ownrun": ("crates/khora-types/src/unimplemented.rs",
               "        (\"assert\", 1) => answers_nothing && matches!(ty(0), Type::Bool),\n",
               "        (\"assert\", 1) => answers_nothing && matches!(ty(0), Type::Bool),\n        (\"contain_run\", 1) => matches!(ty(0), Type::Fn { .. }),\n",
               "containment::a_program_cannot_reach_contain_without_its_rule", "compiler"),
    "ownrun2": ("crates/khora-codegen-llvm/src/lower/calls.rs",
                "                } else if is_intrinsic && core && name == \"contain_run\" {",
                "                } else if is_intrinsic && name == \"contain_run\" {",
                "containment::a_program_cannot_reach_contain_without_its_rule", "compiler"),
    # The checker's rules key on the name written at the call again: an
    # alias of `contain` or `contain_with` is an ordinary function.
    "byspelling": ("crates/khora-types/src/check/calls.rs",
                   "            .map_or(name.as_str(), |origin| origin.name.as_str());\n        if module",
                   "            .map_or(name.as_str(), |_| name.as_str());\n        if module",
                   "containment::a_program_cannot_reach_contain_without_its_rule", "compiler"),
    # `contain` and `contain_with` may be taken as values.
    "asvalue": ("crates/khora-types/src/check/calls.rs",
                "        if self.called_directly.contains(&at) {\n            return;\n        }",
                "        if self.called_directly.contains(&at) || !self.called_directly.contains(&at) {\n            return;\n        }",
                "containment::a_program_cannot_reach_contain_without_its_rule", "compiler"),
    # A program's own bodiless `attempt` and `within_deadline` are the
    # intrinsics again, and a `print` of any answer type is std's. Both
    # guards (checker and backend), as for `ownrun`.
    "ownintrinsics": ("crates/khora-types/src/unimplemented.rs",
                      "    match (name.as_str(), args.len()) {\n        (\"print\", 1) => {\n            answers_nothing &&",
                      "    let answers_nothing = answers_nothing || true;\n    match (name.as_str(), args.len()) {\n        (\"attempt\", 1) => matches!(ty(0), Type::Fn { .. }),\n        (\"within_deadline\", 2) => matches!((ty(0), ty(1)), (Type::Int, Type::Fn { .. })),\n        (\"contain_run\", 1) => matches!(ty(0), Type::Fn { .. }),\n        (\"print\", 1) => {\n            answers_nothing &&",
                      "check::an_own_declaration_of_an_intrinsic_is_not_the_intrinsic", "compiler"),
    "ownintrinsics2": ("crates/khora-codegen-llvm/src/lower/calls.rs",
                       "                let core = module.segments() == [\"std\", \"core\"];",
                       "                let core = !module.segments().is_empty() || module.segments().is_empty();",
                       "check::an_own_declaration_of_an_intrinsic_is_not_the_intrinsic", "compiler"),
    # A `contain` refusal gives the spawn's reason ("two fibers ... race").
    "noreason": ("crates/khora-types/src/check/sharing.rs",
                 "            Crossing::Contain => self.types.why_not_containable(ty),",
                 "            Crossing::Contain => self.types.why_unshareable(ty),",
                 "containment::contain_refuses_what_a_trap_could_leave_half_written", "compiler"),
    # A path that cannot be canonicalized is compared as written (`..` and all).
    "rawpath": ("crates/khora-types/src/forging.rs",
                "    path.canonicalize().is_ok_and(|real| real.starts_with(dir))\n",
                "    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).starts_with(dir)\n",
                "spelled_through_the_std | under_a_directory_only", "compiler"),
    # With no MCP declaration, a `module std::..` snippet is std's own again.
    "mcpundeclared": ("crates/khora-mcp/src/tools.rs",
                      "    khora_db::DependencyFiles::new(&db, Vec::new());\n",
                      "",
                      "a_snippet_named_std_gets_no_standard_library_exemption", "compiler"),
    # The standard library is a module named `std::..` again, not std's files.
    "modline": ("crates/khora-types/src/standard_library.rs",
                "    crate::forging::is_standard_library(db, file)\n",
                "    khora_hir::item_map(db, file).module.as_ref().and_then(|m| m.segments().first()).is_some_and(|f| f == \"std\")\n",
                "check::may_not_call_contain_with", "compiler"),
    # A `KHORA_TRAP_BUDGET` that does not parse falls back in silence.
    "nowarn": ("crates/khora-rt/src/fibertrap.rs",
               "            let _ = writeln!(std::io::stderr(), \"{}\", unreadable_budget(&text));\n",
               "",
               "containment::an_unreadable_trap_budget_is_named_once", "rt"),
    # `Router` does not contain: a trapping handler ends the server.
    "norouter": ("std/net/http_native.kh",
                 "  pub fn new<'er>() -> Router<'er> { { routes: List::Nil, limit: default_limit(), contained: true } }",
                 "  pub fn new<'er>() -> Router<'er> { { routes: List::Nil, limit: default_limit(), contained: false } }",
                 "traps_in_a_server::a_trap_in_a_handler_answers_500_and_the_server_goes_on", "std"),
}

def main():
    what = sys.argv[1]
    if what == "list":
        for k, v in MUTANTS.items():
            print(k, v[4], v[3])
        return
    if what == "restore":
        if not os.path.isdir(SAVE):
            print("nothing saved"); return
        for root, _, files in os.walk(SAVE):
            for f in files:
                src = os.path.join(root, f)
                rel = os.path.relpath(src, SAVE)
                shutil.copyfile(src, os.path.join(TREE, rel))
                print("restored", rel)
        shutil.rmtree(SAVE)
        return
    if os.path.isdir(SAVE):
        sys.exit("a mutant is already applied; restore first")
    names = what.split("+")
    edits = []
    for name in names:
        path, old, new, test, build = MUTANTS[name]
        edits.append((path, old, new))
    # Check every anchor before touching anything, so a bad one changes nothing.
    texts = {}
    for path, old, _ in edits:
        full = os.path.join(TREE, path)
        text = texts.get(path) or open(full, encoding="utf-8").read()
        if text.count(old) != 1:
            sys.exit(f"{what}: anchor matches {text.count(old)} times in {path}")
        texts[path] = text
    for path, old, new in edits:
        full = os.path.join(TREE, path)
        saved = os.path.join(SAVE, path)
        if not os.path.exists(saved):
            os.makedirs(os.path.dirname(saved), exist_ok=True)
            shutil.copyfile(full, saved)
        texts[path] = texts[path].replace(old, new)
        open(full, "w", encoding="utf-8").write(texts[path])
        print(f"applied to {path}")
    print(f"applied {what}; expect red: {MUTANTS[names[-1]][3]}")

main()
