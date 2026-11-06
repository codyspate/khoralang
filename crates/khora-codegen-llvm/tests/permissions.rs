#![cfg(feature = "llvm")]

//! `[permissions]` against the real file system and the real network.
//!
//! **What this guards: a grant that holds for the text of a request and not
//! for where the request goes.** Two holes of that shape were open:
//!
//! - an `fs` grant was matched against the path a program passed, so a
//!   symlink inside a granted tree led out of it and was followed;
//! - the `network` grant was consulted only by `HttpClient::send`, so
//!   `std::net::socket::connect_to`, `connect_by` and `std::net::tls::connect`
//!   dialed any host under `default = "deny"`.
//!
//! Every program here is compiled against the real `std` with
//! `std::permissions::grants` replaced, which is exactly what `khora build`
//! does with a manifest's `[permissions]`, and each is run on both fiber
//! backends.

use crate::harness;

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

const BACKENDS: [&str; 2] = ["threads", "scheduler"];

/// The grants a program is compiled with, as `khora build` writes them.
struct Grants<'a> {
    read: &'a [&'a str],
    write: &'a [&'a str],
    network: &'a [&'a str],
}

/// `std::permissions::grants`, from `grants`, with `listen = ["*"]` -- an
/// absent key with no table -- so a test about something else may listen,
/// in the same shape as the module `khora build` generates.
fn grants_module(grants: &Grants) -> String {
    grants_module_listening(grants, &["*"])
}

/// [`grants_module`], with `listen` as the `[permissions] listen` list.
fn grants_module_listening(grants: &Grants, listen: &[&str]) -> String {
    let list = |patterns: &[&str]| {
        patterns
            .iter()
            .rev()
            .fold("List::Nil".to_string(), |rest, p| format!("List::Cons(\"{p}\", {rest})"))
    };
    format!(
        "module std::permissions::grants;\n\nimport std::core::{{List}};\n\n\
         /// Read.\npub fn fs_read() -> List<String> {{ {} }}\n\
         /// Write.\npub fn fs_write() -> List<String> {{ {} }}\n\
         /// Env.\npub fn env() -> List<String> {{ List::Cons(\"*\", List::Nil) }}\n\
         /// Network.\npub fn network() -> List<String> {{ {} }}\n\
         /// Listen.\npub fn listen() -> List<String> {{ {} }}\n\
         /// Process.\npub fn process() -> List<String> {{ List::Cons(\"*\", List::Nil) }}\n",
        list(grants.read),
        list(grants.write),
        list(grants.network),
        list(listen),
    )
}

/// Every `std` file this host selects, with `grants.kh` replaced by
/// `module` when one is given and left as it is on disk when not.
fn sources_with(db: &KhoraDatabase, dir: &Path, module: Option<String>, main: &str) -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = match (&module, path.file_name().is_some_and(|n| n == "grants.kh")) {
                    (Some(generated), true) => generated.clone(),
                    _ => std::fs::read_to_string(&path).expect("readable"),
                };
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out.push(SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

/// Compiles `main` under `grants` into `CARGO_TARGET_TMPDIR/<name>/program`.
fn build(name: &str, grants: &Grants, main: &str) -> PathBuf {
    build_with(name, Some(grants_module(grants)), main)
}

/// [`build`], with `grants.kh` replaced by `module`, or left as it is on
/// disk -- what a program with no `[permissions]` table compiles with.
fn build_with(name: &str, module: Option<String>, main: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);
    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources_with(&db, &dir, module, main));
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  "));
    }
    exe
}

/// How long any program here may take. Each one does a few local operations
/// and exits, so this is far over what any needs; it exists so that a
/// regression that dials and then waits on a peer fails rather than hangs.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs `exe` in `cwd` on `backend`, answering with what it printed. Killed
/// after [`PATIENCE`], which fails the test.
fn run(exe: &Path, cwd: &Path, backend: &str) -> String {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new(exe)
        .current_dir(cwd)
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting") {
            break status;
        }
        if started.elapsed() > PATIENCE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{backend}`: {} did not finish in {PATIENCE:?}", exe.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let _ = child.stdout.take().expect("stdout").read_to_string(&mut stdout);
    let _ = child.stderr.take().expect("stderr").read_to_string(&mut stderr);
    let text = stdout.replace("\r\n", "\n");
    assert!(status.success(), "`{backend}` exited {:?}:\n{text}\n{stderr}", status.code());
    text
}

// ---------------------------------------------------------------------------
// The file system
// ---------------------------------------------------------------------------

/// Every operation `std::fs` has, through every kind of link: one that stays
/// inside the grant, one that leads out, one to the parent directory, a chain
/// of two, and a directory that is a link. Each line is an operation and what
/// it answered.
#[cfg(unix)]
const FOLLOWING: &str = r#"module demo::main;

import std::core::{List, Raised, Result, attempt, print};
import std::fs::{FsRead, FsWrite, IoError, read_text, write_text};

fn said(e: IoError) -> String {
  match e {
    IoError::Denied(_) => "denied",
    IoError::NotFound(_) => "missing",
    IoError::Failed(_) => "failed",
  }
}

fn read(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => read_text(path)!) {
    Result::Ok(text) => text,
    Result::Err(why) => said(Raised::only(why)),
  }
}

fn sized(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => reads.size(path)!) {
    Result::Ok(_) => "ok",
    Result::Err(why) => said(Raised::only(why)),
  }
}

fn listed(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => reads.read_dir(path)!) {
    Result::Ok(_) => "ok",
    Result::Err(why) => said(Raised::only(why)),
  }
}

fn written(path: String) -> String with { writes: FsWrite } {
  match attempt(fn () => write_text(path, "written")!) {
    Result::Ok(_) => "ok",
    Result::Err(why) => said(Raised::only(why)),
  }
}

fn removed(path: String) -> String with { writes: FsWrite } {
  match attempt(fn () => writes.remove(path)!) {
    Result::Ok(_) => "ok",
    Result::Err(why) => said(Raised::only(why)),
  }
}

fn renamed(from: String, to: String) -> String with { writes: FsWrite } {
  match attempt(fn () => writes.rename(from, to)!) {
    Result::Ok(_) => "ok",
    Result::Err(why) => said(Raised::only(why)),
  }
}

pub fn main() -> Int {
  with { reads: FsRead::real(), writes: FsWrite::real() } {
    print("plain=" + read("granted/ok.txt"));
    print("inside=" + read("granted/inside"));
    print("outside=" + read("granted/outside"));
    print("parent=" + read("granted/up/secret.txt"));
    print("chain=" + read("granted/first"));
    print("door=" + read("granted/door/x.txt"));
    print("size_outside=" + sized("granted/outside"));
    print("list_door=" + listed("granted/door"));
    print("write_new=" + written("granted/new.txt"));
    print("write_inside=" + written("granted/inside"));
    print("write_outside=" + written("granted/outside"));
    print("write_parent=" + written("granted/up/planted.txt"));
    print("write_door=" + written("granted/door/planted.txt"));
    print("remove_parent=" + removed("granted/up/secret.txt"));
    print("rename_from=" + renamed("granted/up/secret.txt", "granted/taken.txt"));
    print("rename_to=" + renamed("granted/new.txt", "granted/door/moved.txt"));
    print("remove_link=" + removed("granted/second"));
  };
  0
}
"#;

/// What [`FOLLOWING`] has to print: inside the tree is allowed, out of it is
/// refused, however the way out is spelled. Removing a link acts on the link
/// itself, which is inside, so it is allowed wherever the link points.
#[cfg(unix)]
const FOLLOWED: &str = "plain=ok
inside=ok
outside=denied
parent=denied
chain=denied
door=denied
size_outside=denied
list_door=denied
write_new=ok
write_inside=ok
write_outside=denied
write_parent=denied
write_door=denied
remove_parent=denied
rename_from=denied
rename_to=denied
remove_link=ok
";

/// The tree [`FOLLOWING`] runs in, made fresh.
///
/// ```text
/// secret.txt         (outside the grant)
/// elsewhere/x.txt    (outside the grant)
/// granted/ok.txt
/// granted/inside   -> ok.txt
/// granted/outside  -> ../secret.txt
/// granted/up       -> ..
/// granted/first    -> second
/// granted/second   -> ../secret.txt
/// granted/door     -> ../elsewhere
/// ```
#[cfg(unix)]
fn linked_tree(at: &Path) {
    use std::os::unix::fs::symlink;
    let _ = std::fs::remove_dir_all(at);
    std::fs::create_dir_all(at.join("granted")).expect("the tree");
    std::fs::create_dir_all(at.join("elsewhere")).expect("the tree");
    std::fs::write(at.join("secret.txt"), "secret").expect("the tree");
    std::fs::write(at.join("elsewhere").join("x.txt"), "elsewhere").expect("the tree");
    std::fs::write(at.join("granted").join("ok.txt"), "ok").expect("the tree");
    let granted = at.join("granted");
    for (link, target) in [
        ("inside", "ok.txt"),
        ("outside", "../secret.txt"),
        ("up", ".."),
        ("first", "second"),
        ("second", "../secret.txt"),
        ("door", "../elsewhere"),
    ] {
        symlink(target, granted.join(link)).expect("a link");
    }
}

/// **A grant is checked against where the path leads.** Unix only: making a
/// symlink on Windows needs a privilege a CI runner does not hold, and the
/// runtime's link-following code on Windows is checked by compilation alone.
#[cfg(unix)]
#[test]
fn a_symlink_cannot_lead_out_of_a_granted_tree() {
    let grants = Grants {
        read: &["granted", "granted/**"],
        write: &["granted", "granted/**"],
        network: &["*"],
    };
    let exe = build("perm_symlinks", &grants, FOLLOWING);
    for backend in BACKENDS {
        let tree = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("perm_symlinks_{backend}"));
        linked_tree(&tree);
        let said = run(&exe, &tree, backend);
        assert_eq!(said, FOLLOWED, "`{backend}`");
        // And the file system agrees: nothing outside the tree was touched.
        assert_eq!(std::fs::read_to_string(tree.join("secret.txt")).expect("still there"), "secret");
        assert!(!tree.join("planted.txt").exists(), "`{backend}`: a write landed through `up`");
        assert!(!tree.join("elsewhere").join("planted.txt").exists(), "`{backend}`: through `door`");
        assert!(!tree.join("elsewhere").join("moved.txt").exists(), "`{backend}`: renamed out");
        assert!(tree.join("granted").join("new.txt").exists(), "`{backend}`: the granted write");
    }
}

/// **The default grants nothing less.** With no `[permissions.fs]`, `**`
/// covers the disk, so every link is followed exactly as the operating system
/// follows it -- the check does not walk the path at all.
///
/// And an absolute path outside the working directory, and `/` itself, are
/// read as they always were. Without the short-circuit for `**` the followed
/// check treats `**` as a tree under the working directory, which refuses
/// both: a program with no `[permissions]` table at all would lose the disk.
#[cfg(unix)]
#[test]
fn an_unrestricted_grant_follows_every_link() {
    let grants = Grants { read: &["**"], write: &["**"], network: &["*"] };
    let exe = build("perm_symlinks_open", &grants, FOLLOWING);
    let tree = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("perm_symlinks_open_tree");
    linked_tree(&tree);
    let said = run(&exe, &tree, "threads");
    assert!(said.contains("outside=secret\n"), "{said}");
    assert!(said.contains("door=elsewhere\n"), "{said}");

    // A file outside the working directory, named absolutely.
    let elsewhere = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("perm_open_elsewhere.txt");
    std::fs::write(&elsewhere, "far").expect("a file elsewhere");
    let program = ABSOLUTE.replace("@FILE@", &elsewhere.to_string_lossy());
    let exe = build("perm_open_absolute", &grants, &program);
    for backend in BACKENDS {
        assert_eq!(run(&exe, &tree, backend), "far=far\nroot=ok\n", "`{backend}`");
    }
}

/// Reads one absolute path and lists `/`.
#[cfg(unix)]
const ABSOLUTE: &str = r#"module demo::main;

import std::core::{Raised, Result, attempt, print};
import std::fs::{FsRead, IoError, read_text};

fn said(e: IoError) -> String {
  match e {
    IoError::Denied(_) => "denied",
    IoError::NotFound(_) => "missing",
    IoError::Failed(_) => "failed",
  }
}

pub fn main() -> Int {
  with { reads: FsRead::real() } {
    print("far=" + (match attempt(fn () => read_text("@FILE@")!) {
      Result::Ok(t) => t,
      Result::Err(e) => said(Raised::only(e)),
    }));
    print("root=" + (match attempt(fn () => reads.read_dir("/")!) {
      Result::Ok(_) => "ok",
      Result::Err(e) => said(Raised::only(e)),
    }));
  };
  0
}
"#;

/// Reads `data/link` and writes through `data/door`, saying what each answered.
#[cfg(unix)]
const STARRED: &str = r#"module demo::main;

import std::core::{Raised, Result, attempt, print};
import std::fs::{FsRead, FsWrite, IoError, read_text, write_text};

fn said(e: IoError) -> String {
  match e {
    IoError::Denied(_) => "denied",
    IoError::NotFound(_) => "missing",
    IoError::Failed(_) => "failed",
  }
}

pub fn main() -> Int {
  with { reads: FsRead::real(), writes: FsWrite::real() } {
    print("read=" + (match attempt(fn () => read_text("data/link")!) {
      Result::Ok(t) => t,
      Result::Err(e) => said(Raised::only(e)),
    }));
    print("write=" + (match attempt(fn () => write_text("data/door/planted.txt", "planted")!) {
      Result::Ok(_) => "ok",
      Result::Err(e) => said(Raised::only(e)),
    }));
  };
  0
}
"#;

/// **The directory a grant leads to is a place, not a pattern.** A relative
/// grant is followed from the working directory, and the absolute answer was
/// pasted in front of the grant's glob as if it were glob too: from a
/// directory named `proj*x`, `data/**` became `/…/proj*x/data/**`, whose `*`
/// covered a sibling `projEVILx`; from `a**z`, it covered a whole subtree of
/// the parent. A link inside `data` then reached either.
///
/// `cwd` is the directory to run in, named with the wildcard; `outside` is
/// where the links lead, relative to the run's root.
#[cfg(unix)]
fn starred_tree(root: &Path, cwd: &str, outside: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::symlink;
    let _ = std::fs::remove_dir_all(root);
    let here = root.join(cwd);
    let there = root.join(outside);
    std::fs::create_dir_all(here.join("data")).expect("the tree");
    std::fs::create_dir_all(&there).expect("the tree");
    std::fs::write(there.join("secret.txt"), "secret").expect("the tree");
    symlink(there.join("secret.txt"), here.join("data").join("link")).expect("a link");
    symlink(&there, here.join("data").join("door")).expect("a link");
    (here, there)
}

#[cfg(unix)]
#[test]
fn a_wildcard_in_the_working_directory_is_not_a_wildcard() {
    let grants = Grants { read: &["data/**"], write: &["data/**"], network: &["*"] };
    let exe = build("perm_starred", &grants, STARRED);
    for backend in BACKENDS {
        for (cwd, outside) in [("proj*x", "projEVILx/data"), ("a**z", "aQ/deep/er/z/data")] {
            let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("perm_starred_{backend}"));
            let (here, there) = starred_tree(&root, cwd, outside);
            let said = run(&exe, &here, backend);
            assert_eq!(said, "read=denied\nwrite=denied\n", "`{backend}` from `{cwd}`");
            assert!(!there.join("planted.txt").exists(), "`{backend}` from `{cwd}`: planted outside");
        }
    }
}

/// Reads `granted\evil/f.txt`, which off Windows is a file in a directory
/// named `granted\evil` -- beside `granted`, not in it.
#[cfg(unix)]
const BACKSLASHED: &str = r#"module demo::main;

import std::core::{Raised, Result, attempt, print};
import std::fs::{FsRead, IoError, read_text};

pub fn main() -> Int {
  with { reads: FsRead::real() } {
    print("read=" + (match attempt(fn () => read_text("granted\\evil/f.txt")!) {
      Result::Ok(t) => t,
      Result::Err(e) => match Raised::only(e) {
        IoError::Denied(_) => "denied",
        IoError::NotFound(_) => "missing",
        IoError::Failed(_) => "failed",
      },
    }));
  };
  0
}
"#;

/// **A backslash is a separator only on Windows.** Elsewhere it is an
/// ordinary byte in a file name, so `granted\evil` is a sibling of
/// `granted`; reading it as `granted/evil` let `granted/**` cover a
/// directory the manifest never named.
#[cfg(unix)]
#[test]
fn a_backslash_is_not_a_separator_off_windows() {
    let grants = Grants { read: &["granted/**"], write: &["granted/**"], network: &["*"] };
    let exe = build("perm_backslash", &grants, BACKSLASHED);
    let tree = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("perm_backslash_tree");
    let _ = std::fs::remove_dir_all(&tree);
    std::fs::create_dir_all(tree.join("granted")).expect("the tree");
    std::fs::create_dir_all(tree.join("granted\\evil")).expect("the tree");
    std::fs::write(tree.join("granted\\evil").join("f.txt"), "sibling").expect("the tree");
    for backend in BACKENDS {
        assert_eq!(run(&exe, &tree, backend), "read=denied\n", "`{backend}`");
    }
}

/// Reads a file through a link loop and one under a directory nobody may
/// search, both inside the grant, and one outside it.
#[cfg(unix)]
const UNFOLLOWABLE: &str = r#"module demo::main;

import std::core::{Raised, Result, attempt, print};
import std::fs::{FsRead, IoError, read_text};

fn read(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => read_text(path)!) {
    Result::Ok(t) => t,
    Result::Err(e) => match Raised::only(e) {
      IoError::Denied(_) => "denied",
      IoError::NotFound(_) => "missing",
      IoError::Failed(_) => "failed",
    },
  }
}

pub fn main() -> Int {
  with { reads: FsRead::real() } {
    print("loop=" + read("granted/loop"));
    print("locked=" + read("granted/locked/f.txt"));
    print("outside=" + read("secret.txt"));
  };
  0
}
"#;

/// **`Denied` is the manifest's answer and only the manifest's.** A path
/// inside the grant whose links cannot be followed -- a loop, a directory this
/// process may not search -- cannot be called granted, so it is refused; but
/// the cause is the file system, and the answer is `Failed`, the one it would
/// get with no `[permissions]` at all. Reporting `Denied` sent the reader to
/// `khora.toml` for something no line there could fix.
///
/// The locked directory is skipped as root, which searches every directory
/// whatever its mode.
#[cfg(unix)]
#[test]
fn a_path_that_cannot_be_followed_failed_rather_than_denied() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let grants = Grants { read: &["granted/**"], write: &["granted/**"], network: &["*"] };
    let exe = build("perm_unfollowable", &grants, UNFOLLOWABLE);
    let tree = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("perm_unfollowable_tree");
    let locked = tree.join("granted").join("locked");
    if locked.exists() {
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_dir_all(&tree);
    std::fs::create_dir_all(&locked).expect("the tree");
    std::fs::write(locked.join("f.txt"), "locked").expect("the tree");
    std::fs::write(tree.join("secret.txt"), "secret").expect("the tree");
    symlink("loop", tree.join("granted").join("loop")).expect("a link");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("locking");
    // Root searches a mode-000 directory anyway, so the lock proves nothing there.
    let as_root = std::fs::read_dir(&locked).is_ok();
    let saids: Vec<String> = BACKENDS.iter().map(|backend| run(&exe, &tree, backend)).collect();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlocking");
    for (backend, said) in BACKENDS.iter().zip(saids) {
        assert!(said.contains("loop=failed\n"), "`{backend}`: {said}");
        assert!(said.contains("outside=denied\n"), "`{backend}`: {said}");
        if as_root {
            eprintln!("skipping the locked-directory half: running as root, which searches it anyway");
        } else {
            assert!(said.contains("locked=failed\n"), "`{backend}`: {said}");
        }
    }
}

// ---------------------------------------------------------------------------
// The network
// ---------------------------------------------------------------------------

/// Dials `PORT` with both of `std::net::socket`'s connects and says what
/// each did: connected, or which `SocketError` it raised.
const DIALING: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{SocketError, connect_by, connect_to, shut, start};

fn said(handle: Int) -> String {
  shut(handle);
  if handle >= 0 { "connected" } else { "a negative handle" }
}

fn why(e: SocketError) -> String {
  match e {
    SocketError::Denied(host) => "denied " + host,
    SocketError::Unreachable(_) => "unreachable",
    SocketError::TimedOut(_) => "timed out",
  }
}

fn to() -> String raises SocketError { said(connect_to("127.0.0.1", PORT)!) }

fn by() -> String raises SocketError { said(connect_by("127.0.0.1", PORT, 0 - 1)!) }

pub fn main() -> Int {
  if start() {
    print("connect_to=" + (to()! catch { e => why(e) }));
    print("connect_by=" + (by()! catch { e => why(e) }));
  } else {
    print("no socket layer");
  };
  0
}
"#;

/// How many connections `listener` has waiting, without waiting for one.
fn waiting(listener: &TcpListener) -> usize {
    listener.set_nonblocking(true).expect("non-blocking");
    let mut count = 0;
    loop {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return count,
            Err(e) => panic!("accept: {e}"),
        }
    }
}

/// **`connect_to` and `connect_by` consult `[permissions] network`.** Under a
/// grant that does not name the host, both raise `SocketError::Denied` with
/// the `host:port` a grant would name, and neither dials: the listener has
/// nothing waiting once the program has exited. Under one that does, both
/// connect.
#[test]
fn a_raw_connect_is_refused_a_host_the_manifest_does_not_grant() {
    for backend in BACKENDS {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let program = DIALING.replace("PORT", &port.to_string());
        let refused = format!("connect_to=denied 127.0.0.1:{port}\nconnect_by=denied 127.0.0.1:{port}\n");

        let denied = Grants { read: &["**"], write: &["**"], network: &[] };
        let exe = build(&format!("perm_dial_denied_{backend}"), &denied, &program);
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(said, refused, "`{backend}`");
        assert_eq!(waiting(&listener), 0, "`{backend}`: a refused host must not be dialed");

        let other_port = format!("127.0.0.1:{}", port.wrapping_add(1));
        let elsewhere = Grants { read: &["**"], write: &["**"], network: &[&other_port] };
        let exe = build(&format!("perm_dial_other_{backend}"), &elsewhere, &program);
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(said, refused, "`{backend}`: another port");
        assert_eq!(waiting(&listener), 0, "`{backend}`: another port's grant is not this one's");

        let granted = Grants { read: &["**"], write: &["**"], network: &["127.0.0.1"] };
        let exe = build(&format!("perm_dial_granted_{backend}"), &granted, &program);
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(said, "connect_to=connected\nconnect_by=connected\n", "`{backend}`");
        assert_eq!(waiting(&listener), 2, "`{backend}`: a granted host is dialed");
    }
}

/// Dials `::1` on `PORT` twice, once as the address is written and once in
/// the brackets a URL writes it in.
const DIALING_V6: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{connect_to, shut, start};

fn said(handle: Int) -> String {
  if handle >= 0 {
    shut(handle);
    "connected"
  } else {
    "refused"
  }
}

pub fn main() -> Int {
  if start() {
    print("bare=" + said(connect_to("::1", PORT)! catch { _ => -1 }));
    print("bracketed=" + said(connect_to("[::1]", PORT)! catch { _ => -1 }));
  } else {
    print("no socket layer");
  };
  0
}
"#;

/// **An IPv6 literal can be granted.** A manifest writes it in brackets, as a
/// URL does -- `[::1]:5432`, `[::1]:*`, `[::1]` -- and `connect_to` accepts
/// the address with or without them. Without brackets the last colon was
/// taken for the port's, so no spelling of `::1` in a manifest covered a
/// dial to it, and a bracketed host passed the check and could not be
/// dialed.
///
/// Skipped, saying so, on a machine with no IPv6 loopback.
#[test]
fn an_ipv6_literal_can_be_granted() {
    let listener = match TcpListener::bind(("::1", 0)) {
        Ok(listener) => listener,
        Err(why) => {
            eprintln!("skipping: this machine has no IPv6 loopback to listen on ({why})");
            return;
        }
    };
    let port = listener.local_addr().expect("an address").port();
    let program = DIALING_V6.replace("PORT", &port.to_string());
    for (grant, expected, dialed) in [
        (format!("[::1]:{port}"), "bare=connected\nbracketed=connected\n", 2),
        ("[::1]:*".to_string(), "bare=connected\nbracketed=connected\n", 2),
        ("[::1]".to_string(), "bare=connected\nbracketed=connected\n", 2),
        (format!("[::1]:{}", port.wrapping_add(1)), "bare=refused\nbracketed=refused\n", 0),
    ] {
        let grants = Grants { read: &["**"], write: &["**"], network: &[&grant] };
        let name = format!("perm_dial_v6_{}", grant.replace([':', '[', ']', '*'], "_"));
        let exe = build(&name, &grants, &program);
        for backend in BACKENDS {
            let said = run(&exe, exe.parent().expect("a directory"), backend);
            assert_eq!(said, expected, "`{backend}` under `{grant}`");
            assert_eq!(waiting(&listener), dialed, "`{backend}` under `{grant}`");
        }
    }
}

/// **A denial is told apart from a failure**, so a caller can tell a line in
/// `khora.toml` from a host that is down: dialing `PORT`, which the grant
/// does not name, raises `Denied`, and dialing `CLOSED`, which it names and
/// where nothing listens, raises `Unreachable`.
const TELLING: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{SocketError, connect_to, shut, start};

fn why(e: SocketError) -> String {
  match e {
    SocketError::Denied(host) => "denied " + host,
    SocketError::Unreachable(host) => "unreachable " + host,
    SocketError::TimedOut(host) => "timed out " + host,
  }
}

fn dialed(port: Int) -> String raises SocketError {
  let h = connect_to("127.0.0.1", port)!;
  shut(h);
  if h >= 0 { "connected" } else { "a negative handle" }
}

pub fn main() -> Int {
  start();
  print(dialed(PORT)! catch { e => why(e) });
  print(dialed(CLOSED)! catch { e => why(e) });
  0
}
"#;

#[test]
fn a_denied_connect_is_told_apart_from_a_failed_one() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let closed = {
        let gone = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        gone.local_addr().expect("an address").port()
    };
    let program = TELLING.replace("CLOSED", &closed.to_string()).replace("PORT", &port.to_string());
    let only_closed = format!("127.0.0.1:{closed}");
    let grants = Grants { read: &["**"], write: &["**"], network: &[&only_closed] };
    let exe = build("perm_dial_told", &grants, &program);
    for backend in BACKENDS {
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(
            said,
            format!("denied 127.0.0.1:{port}\nunreachable 127.0.0.1:{closed}\n"),
            "`{backend}`"
        );
    }
    assert_eq!(waiting(&listener), 0);
}

/// `std::net::tls::connect` dials for itself, so it checks the grant for
/// itself, and says `Denied` with the `host:port` a grant would name.
///
/// The connect has a deadline because the listener never answers a
/// handshake: a `connect` that dialed would otherwise wait on it until the
/// test's watchdog, and a deadline makes that a quick `timed out` instead.
const SECURING: &str = r#"module demo::main;

import std::core::{Raised, Result, Scope, attempt, print};
import std::net::socket::{start};
import std::net::tls::{TlsError, client, connect_by, shut};

extern fn khora_monotonic_millis() -> Int;

pub fn main() -> () {
  with { scope: Scope::root() } {
    start();
    let by = khora_monotonic_millis() + 2000;
    match attempt(fn () => connect_by(client()!, "127.0.0.1", PORT, by)!) {
      Result::Ok(session) => { shut(session); print("connected") },
      Result::Err(why) => match Raised::only(why) {
        TlsError::Denied(host) => print("denied " + host),
        TlsError::Unreachable(_) => print("unreachable"),
        TlsError::TimedOut => print("timed out"),
        TlsError::Handshake => print("handshake"),
        TlsError::BadCertificate => print("bad certificate"),
        TlsError::BadKey => print("bad key"),
      },
    }
  }
}
"#;

#[test]
fn a_tls_connect_is_refused_a_host_the_manifest_does_not_grant() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let program = SECURING.replace("PORT", &port.to_string());
    let denied = Grants { read: &["**"], write: &["**"], network: &["example.com"] };
    let exe = build("perm_tls_denied", &denied, &program);
    for backend in BACKENDS {
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(said, format!("denied 127.0.0.1:{port}\n"), "`{backend}`");
        assert_eq!(waiting(&listener), 0, "`{backend}`: a refused host must not be dialed");
    }
}

// ---------------------------------------------------------------------------
// The second review: shapes of `beneath`, probes, a link a grant names
// ---------------------------------------------------------------------------

/// One line per argument: `op:path` runs that operation and prints what it
/// answered. The paths are spliced in by [`operating`], so each test names
/// exactly the operations it asserts.
#[cfg(unix)]
const OPERATIONS: &str = r#"module demo::main;

import std::core::{List, Raised, Result, attempt, print};
import std::fs::{FsRead, FsWrite, IoError, parent, read_text, write_text};

fn said(e: IoError) -> String {
  match e {
    IoError::Denied(_) => "denied",
    IoError::NotFound(_) => "missing",
    IoError::Failed(_) => "failed",
  }
}

fn yes(b: Bool) -> String { if b { "true" } else { "false" } }

fn read(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => read_text(path)!) { Result::Ok(t) => t, Result::Err(e) => said(Raised::only(e)) }
}

fn listed(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => reads.read_dir(path)!) { Result::Ok(_) => "ok", Result::Err(e) => said(Raised::only(e)) }
}

fn is_dir(path: String) -> String with { reads: FsRead } {
  match attempt(fn () => reads.is_dir(path)!) { Result::Ok(b) => yes(b), Result::Err(e) => said(Raised::only(e)) }
}

fn written(path: String) -> String with { writes: FsWrite } {
  match attempt(fn () => write_text(path, "planted")!) { Result::Ok(_) => "ok", Result::Err(e) => said(Raised::only(e)) }
}

fn removed(path: String) -> String with { writes: FsWrite } {
  match attempt(fn () => writes.remove(path)!) { Result::Ok(_) => "ok", Result::Err(e) => said(Raised::only(e)) }
}

pub fn main() -> Int {
  with { reads: FsRead::real(), writes: FsWrite::real() } {
@CALLS@  };
  0
}
"#;

/// [`OPERATIONS`] with one `print` per `(label, op, path)`.
#[cfg(unix)]
fn operating(ops: &[(&str, &str, &str)]) -> String {
    let calls: String = ops
        .iter()
        .map(|(label, op, path)| {
            let call = match *op {
                "parent" => format!("match parent(\"{path}\") {{ Option::Some(p) => p, Option::None => \"none\" }}"),
                "exists" => format!("match attempt(fn () => reads.exists(\"{path}\")!) {{ Result::Ok(b) => yes(b), Result::Err(e) => said(Raised::only(e)) }}"),
                _ => format!("{op}(\"{path}\")"),
            };
            format!("    print(\"{label}=\" + {call});\n")
        })
        .collect();
    OPERATIONS.replace("@CALLS@", &calls).replace(
        "import std::core::{List, Raised, Result, attempt, print};",
        "import std::core::{List, Option, Raised, Result, attempt, print};",
    )
}

/// A fresh directory under the test's temporary root.
#[cfg(unix)]
fn fresh(name: &str) -> PathBuf {
    let at = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&at);
    std::fs::create_dir_all(&at).expect("a tree");
    at
}

/// **A grant's place is a directory, not a prefix of a name.** `data/**`
/// followed is `/…/data/` with its slash, so a sibling `data2` -- whose real
/// path begins with the same bytes -- is outside it. Links in `data` into
/// `data2`, read and written through, are refused; `data` itself still
/// works. Comparing against the place without its `/` covered `data2`.
#[cfg(unix)]
#[test]
fn a_sibling_whose_name_extends_the_grant_is_outside_it() {
    use std::os::unix::fs::symlink;
    let grants = Grants { read: &["data/**"], write: &["data/**"], network: &["*"] };
    let program = operating(&[
        ("own", "read", "data/ok.txt"),
        ("file", "read", "data/l_sib"),
        ("dir", "read", "data/d_sib/secret.txt"),
        ("write", "written", "data/d_sib/planted.txt"),
    ]);
    let exe = build("perm_sibling_prefix", &grants, &program);
    for backend in BACKENDS {
        let tree = fresh(&format!("perm_sibling_prefix_{backend}"));
        std::fs::create_dir_all(tree.join("data")).expect("data");
        std::fs::create_dir_all(tree.join("data2")).expect("data2");
        std::fs::write(tree.join("data").join("ok.txt"), "ok").expect("a file");
        std::fs::write(tree.join("data2").join("secret.txt"), "sibling").expect("a file");
        symlink("../data2/secret.txt", tree.join("data").join("l_sib")).expect("a link");
        symlink("../data2", tree.join("data").join("d_sib")).expect("a link");
        assert_eq!(
            run(&exe, &tree, backend),
            "own=ok\nfile=denied\ndir=denied\nwrite=denied\n",
            "`{backend}`"
        );
        assert!(!tree.join("data2").join("planted.txt").exists(), "`{backend}`: planted in `data2`");
    }
}

/// **A grant with no glob names one entry, not every name it begins.**
/// Under `data` and `logs/**`, a link `logs/l_sib -> ../data2/secret.txt`
/// leads to `data2/secret.txt`, which no grant names: `data` is the
/// directory `data` and nothing whose path merely starts with it. Matching a
/// literal grant by prefix covered `data2` and everything below `data`.
#[cfg(unix)]
#[test]
fn a_literal_grant_names_one_entry() {
    use std::os::unix::fs::symlink;
    let grants = Grants { read: &["data", "logs/**"], write: &["data", "logs/**"], network: &["*"] };
    let program = operating(&[
        ("dir", "listed", "data"),
        ("sibling", "read", "logs/l_sib"),
        ("inside", "read", "logs/l_in"),
    ]);
    let exe = build("perm_literal_entry", &grants, &program);
    for backend in BACKENDS {
        let tree = fresh(&format!("perm_literal_entry_{backend}"));
        for dir in ["data", "data2", "logs"] {
            std::fs::create_dir_all(tree.join(dir)).expect("a directory");
        }
        std::fs::write(tree.join("data").join("ok.txt"), "ok").expect("a file");
        std::fs::write(tree.join("data2").join("secret.txt"), "sibling").expect("a file");
        symlink("../data2/secret.txt", tree.join("logs").join("l_sib")).expect("a link");
        symlink("../data/ok.txt", tree.join("logs").join("l_in")).expect("a link");
        assert_eq!(
            run(&exe, &tree, backend),
            "dir=ok\nsibling=denied\ninside=denied\n",
            "`{backend}`"
        );
    }
}

/// **`./**` is this directory's tree, not the disk.** Only `**` as written
/// is unrestricted; `./**` is followed and `..`-checked like `./data/**`, so a
/// `..`, an absolute path elsewhere and a link out are refused, and the
/// working directory's own files are read.
#[cfg(unix)]
#[test]
fn a_dot_globstar_is_the_working_directorys_tree() {
    use std::os::unix::fs::symlink;
    let tree = fresh("perm_dot_globstar");
    let cwd = tree.join("proj");
    std::fs::create_dir_all(&cwd).expect("the working directory");
    std::fs::write(cwd.join("ok.txt"), "ok").expect("a file");
    std::fs::write(tree.join("out.txt"), "outside").expect("a file");
    symlink("../out.txt", cwd.join("l_out")).expect("a link");
    let absolute = tree.join("out.txt");
    let grants = Grants { read: &["./**"], write: &["./**"], network: &["*"] };
    let program = operating(&[
        ("own", "read", "ok.txt"),
        ("up", "read", "../out.txt"),
        ("absolute", "read", &absolute.to_string_lossy()),
        ("link", "read", "l_out"),
    ]);
    let exe = build("perm_dot_globstar", &grants, &program);
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, &cwd, backend),
            "own=ok\nup=denied\nabsolute=denied\nlink=denied\n",
            "`{backend}`"
        );
    }
}

/// **`data/` and `data` are one question.** The followed check sees the
/// directory without its trailing slash; the text check has to as well, or
/// `read_dir("data/")` answers differently from `read_dir("data")`. Under
/// `data/**`, which does not cover `data` itself, both are refused; under
/// `data`, both are listed.
#[cfg(unix)]
#[test]
fn a_trailing_slash_does_not_change_the_answer() {
    let program = operating(&[("bare", "listed", "data"), ("slashed", "listed", "data/")]);
    for (grant, expected) in [("data/**", "bare=denied\nslashed=denied\n"), ("data", "bare=ok\nslashed=ok\n")] {
        let grants = Grants { read: &[grant], write: &[grant], network: &["*"] };
        let exe = build(&format!("perm_trailing_{}", grant.replace(['/', '*'], "_")), &grants, &program);
        for backend in BACKENDS {
            let tree = fresh(&format!("perm_trailing_tree_{backend}"));
            std::fs::create_dir_all(tree.join("data")).expect("data");
            assert_eq!(run(&exe, &tree, backend), expected, "`{backend}` under `{grant}`");
        }
    }
}

/// **`exists` and `is_dir` answer `false` for a path that cannot be
/// followed**, as their `false` promises: the operating system would not open
/// it. A link loop inside the grant is that, and it is not the manifest's
/// refusal, so it is neither `Denied` nor an error. A path outside the grant
/// is still `Denied`.
#[cfg(unix)]
#[test]
fn a_probe_of_an_unfollowable_path_is_false() {
    use std::os::unix::fs::symlink;
    let grants = Grants { read: &["granted/**"], write: &["granted/**"], network: &["*"] };
    let program = operating(&[
        ("exists", "exists", "granted/loop"),
        ("is_dir", "is_dir", "granted/loop"),
        ("outside", "exists", "secret.txt"),
        ("present", "exists", "granted/ok.txt"),
    ]);
    let exe = build("perm_probe_unfollowable", &grants, &program);
    for backend in BACKENDS {
        let tree = fresh(&format!("perm_probe_unfollowable_{backend}"));
        std::fs::create_dir_all(tree.join("granted")).expect("granted");
        std::fs::write(tree.join("granted").join("ok.txt"), "ok").expect("a file");
        std::fs::write(tree.join("secret.txt"), "secret").expect("a file");
        symlink("loop", tree.join("granted").join("loop")).expect("a link");
        assert_eq!(
            run(&exe, &tree, backend),
            "exists=false\nis_dir=false\noutside=denied\npresent=true\n",
            "`{backend}`"
        );
    }
}

/// **Removing a link that a literal grant names is allowed, wherever it
/// points.** `remove` acts on the name and follows only the directories
/// above it; the grant naming that name has to be followed the same way, or
/// `ldir` the grant becomes the link's target and `ldir` the link is no
/// longer it. The target is untouched; the link is gone.
#[cfg(unix)]
#[test]
fn a_link_a_literal_grant_names_can_be_removed() {
    use std::os::unix::fs::symlink;
    let grants = Grants { read: &["ldir"], write: &["ldir"], network: &["*"] };
    let program = operating(&[("listed", "listed", "ldir"), ("removed", "removed", "ldir")]);
    let exe = build("perm_literal_link_remove", &grants, &program);
    for backend in BACKENDS {
        let tree = fresh(&format!("perm_literal_link_remove_{backend}"));
        std::fs::create_dir_all(tree.join("data2")).expect("data2");
        std::fs::write(tree.join("data2").join("keep.txt"), "kept").expect("a file");
        symlink("data2", tree.join("ldir")).expect("a link");
        assert_eq!(run(&exe, &tree, backend), "listed=ok\nremoved=ok\n", "`{backend}`");
        assert!(std::fs::symlink_metadata(tree.join("ldir")).is_err(), "`{backend}`: the link is gone");
        assert!(tree.join("data2").join("keep.txt").exists(), "`{backend}`: its target is not");
    }
}

/// **Off Windows `\` is part of a name, for `parent` as for the grants.**
/// `parent("a\\b/c")` is `a\\b`: the one separator in it is the `/`.
/// Splitting at the `\` as well gave the path functions a reading of a path
/// that the permission check did not share.
#[cfg(unix)]
#[test]
fn parent_splits_where_the_permission_check_does() {
    let program = operating(&[("of_mixed", "parent", "a\\\\b/c"), ("of_backslashed", "parent", "a\\\\b")]);
    let grants = Grants { read: &["**"], write: &["**"], network: &["*"] };
    let exe = build("perm_parent_backslash", &grants, &program);
    let here = fresh("perm_parent_backslash_tree");
    for backend in BACKENDS {
        assert_eq!(run(&exe, &here, backend), "of_mixed=a\\b\nof_backslashed=none\n", "`{backend}`");
    }
}

/// Dials `127.0.0.1` with hosts that carry brackets, saying how each answered.
const BRACKETED: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{SocketError, connect_to, shut, start};

fn said(host: String) -> String {
  dialed(host)! catch {
    SocketError::Denied(_) => "denied",
    SocketError::Unreachable(_) => "failed",
    SocketError::TimedOut(_) => "failed",
  }
}

fn dialed(host: String) -> String raises SocketError {
  shut(connect_to(host, PORT)!);
  "connected"
}

pub fn main() -> Int {
  start();
  print("junk=" + said("[127.0.0.1]:1"));
  print("half=" + said("[127.0.0.1"));
  print("doubled=" + said("[[127.0.0.1]]"));
  print("empty=" + said("[]"));
  print("plain=" + said("127.0.0.1"));
  0
}
"#;

/// **A host with a bracket that is not exactly `[addr]` is refused before
/// anything is looked up.** Under a grant of `127.0.0.1` with no port, the
/// host `[127.0.0.1]:1` made the subject `[127.0.0.1]:1:<port>`, which the
/// bracket parser read as the name `127.0.0.1` with the port text `1:<port>`
/// -- covered -- and the name went to the resolver as written. Each spelling
/// here raises `SocketError::Denied`, and nothing reaches the listener.
#[test]
fn a_host_with_stray_brackets_is_denied() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    let port = listener.local_addr().expect("an address").port();
    let program = BRACKETED.replace("PORT", &port.to_string());
    let grants = Grants { read: &["**"], write: &["**"], network: &["[127.0.0.1]"] };
    let exe = build("perm_dial_brackets", &grants, &program);
    for backend in BACKENDS {
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(said, "junk=denied\nhalf=denied\ndoubled=denied\nempty=denied\nplain=connected\n", "`{backend}`");
        assert_eq!(waiting(&listener), 1, "`{backend}`: only the plain host is dialed");
    }
}

/// Dials `::1` on `OTHER`, a port the grant does not name.
const EMPTY_PORT: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{connect_to, shut, start};

pub fn main() -> Int {
  start();
  let h = connect_to("::1", OTHER)! catch { _ => -1 };
  print(if h >= 0 { shut(h); "connected" } else { "refused" });
  0
}
"#;

/// **A grant ending in `:` covers no port.** A manifest refuses the spelling
/// when it loads; the matcher in `std` reads one that reached it some other
/// way as a port nobody grants. It read it as "no port, every port", so
/// `[::1]:` let a connect to any port of `::1` through. Skipped, saying so,
/// on a machine with no IPv6 loopback.
#[test]
fn a_grant_with_an_empty_port_covers_no_port() {
    let listener = match TcpListener::bind(("::1", 0)) {
        Ok(listener) => listener,
        Err(why) => {
            eprintln!("skipping: this machine has no IPv6 loopback to listen on ({why})");
            return;
        }
    };
    let port = listener.local_addr().expect("an address").port();
    let program = EMPTY_PORT.replace("OTHER", &port.to_string());
    let grants = Grants { read: &["**"], write: &["**"], network: &["[::1]:"] };
    let exe = build("perm_dial_empty_port", &grants, &program);
    for backend in BACKENDS {
        assert_eq!(run(&exe, exe.parent().expect("a directory"), backend), "refused\n", "`{backend}`");
        assert_eq!(waiting(&listener), 0, "`{backend}`: nothing dialed");
    }
}

/// Raises a denial for IPv6 hosts written with and without brackets, a name,
/// and an IPv4 address, printing each `SocketError` through `${..}`; then
/// compares cases with `==`.
const NAMING: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{SocketError, connect_by, connect_to, shut, start};

fn to(host: String) -> String raises SocketError {
  shut(connect_to(host, 5432)!);
  "connected"
}

fn by(host: String) -> String raises SocketError {
  shut(connect_by(host, 5432, 0 - 1)!);
  "connected"
}

pub fn main() -> Int {
  start();
  print(to("::1")! catch { e => "${e}" });
  print(by("::1")! catch { e => "${e}" });
  print(to("[::1]")! catch { e => "${e}" });
  print(to("db.internal")! catch { e => "${e}" });
  print(to("127.0.0.1")! catch { e => "${e}" });
  let caught = to("::1")! catch { e => if e == SocketError::Denied("[::1]:5432") { "equal" } else { "different" } };
  print(caught);
  print(if SocketError::Unreachable("a:1") == SocketError::TimedOut("a:1") { "equal" } else { "different" });
  print(if SocketError::Unreachable("a:1") == SocketError::Unreachable("a:2") { "equal" } else { "different" });
  0
}
"#;

/// **A `SocketError` names its host as a grant would, prints, and compares.**
/// A bare IPv6 literal is bracketed -- `[::1]:5432`, the spelling
/// `[permissions] network` needs -- and a bracketed one is not bracketed
/// twice; a name and an IPv4 address are `host:port`. Each prints through
/// `${e}` as its case, and `==` tells cases and hosts apart. Under a grant
/// of nothing, so every dial is refused before anything is looked up.
#[test]
fn a_socket_error_names_its_host_prints_and_compares() {
    let grants = Grants { read: &["**"], write: &["**"], network: &[] };
    let exe = build("perm_socket_error_naming", &grants, NAMING);
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, exe.parent().expect("a directory"), backend),
            "SocketError::Denied([::1]:5432)\n\
             SocketError::Denied([::1]:5432)\n\
             SocketError::Denied([::1]:5432)\n\
             SocketError::Denied(db.internal:5432)\n\
             SocketError::Denied(127.0.0.1:5432)\n\
             equal\ndifferent\ndifferent\n",
            "`{backend}`"
        );
    }
}

// ---------------------------------------------------------------------------
// Listening
// ---------------------------------------------------------------------------

/// `khora_net_local_port`'s declaration, whose handle is an `I32` off
/// Windows and an `Int` on it, and a `local_port` over it.
#[cfg(not(windows))]
const LOCAL_PORT: &str = "extern fn khora_net_local_port(handle: I32) -> Int;\n\
                          fn local_port(socket: Int) -> Int { khora_net_local_port(I32::of(socket)) }";
#[cfg(windows)]
const LOCAL_PORT: &str = "extern fn khora_net_local_port(handle: Int) -> Int;\n\
                          fn local_port(socket: Int) -> Int { khora_net_local_port(socket) }";

/// Tries `listen_on` on each of `PORTS` and prints what each did: the port
/// it got, read back from the socket, or the `SocketError` it raised. Then
/// it dials `CONTROL` and waits there, holding whatever it bound, until the
/// test closes that connection -- so the test can look at the ports while
/// the program is alive.
const LISTENING: &str = r#"module demo::main;

import std::core::{Array, List, print};
import std::net::socket::{SocketError, connect_to, listen_on, receive, start};

@LOCAL_PORT@

fn bound(port: Int) -> String raises SocketError {
  let socket = listen_on(port)!;
  if socket < 0 { "failed" } else { "listening ${Int::to_string(local_port(socket))}" }
}

pub fn main() -> Int {
  start();
  let mut ports: List<Int> = @PORTS@;
  let mut going = true;
  while going {
    match ports {
      List::Nil => going = false,
      List::Cons(port, rest) => {
        print(bound(port)! catch { e => "${e}" });
        ports = rest
      },
    }
  };
  let control = connect_to("127.0.0.1", @CONTROL@)! catch { _ => -1 };
  let buffer: Array<U8> = Array::new(1, 0);
  let _ = receive(control, buffer);
  0
}
"#;

/// A port nothing is listening on, found by binding port 0 and letting it
/// go. A test that loses the race for it to another program fails to bind
/// it and says so; it does not pass.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port()
}

/// [`LISTENING`] for `ports`, holding until `CONTROL` closes.
fn listening(ports: &[u16], control: u16) -> String {
    let list = ports
        .iter()
        .rev()
        .fold("List::Nil".to_string(), |rest, p| format!("List::Cons({p}, {rest})"));
    LISTENING
        .replace("@LOCAL_PORT@", LOCAL_PORT)
        .replace("@PORTS@", &list)
        .replace("@CONTROL@", &control.to_string())
}

/// Whether this process can bind `port` on every interface, which is what
/// `listen_on` binds. False while a live listener holds it.
fn can_bind(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// Runs `exe` on `backend` until it has printed `lines` lines, hands them to
/// `look` while the program is still holding what it bound, then lets it go
/// and waits for it. Killed after [`PATIENCE`], which fails the test.
fn while_held(exe: &Path, backend: &str, control: &TcpListener, lines: usize, look: impl FnOnce(&[String])) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut child = Command::new(exe)
        .current_dir(exe.parent().expect("a directory"))
        .env("KHORA_FIBERS", backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the program should run");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut said = Vec::new();
    for _ in 0..lines {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap_or(0) == 0 {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{backend}`: the program stopped after printing {said:?}");
        }
        said.push(line.trim_end().to_string());
    }
    // The program dials the control port after its last line; until the
    // test accepts and closes that, everything it bound stays bound.
    control.set_nonblocking(true).expect("non-blocking");
    let started = std::time::Instant::now();
    let held = loop {
        match control.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock && started.elapsed() < PATIENCE => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`{backend}`: the program never dialed back: {e}; it said {said:?}");
            }
        }
    };
    look(&said);
    drop(held);
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting") {
            break status;
        }
        if started.elapsed() > PATIENCE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{backend}`: the program did not finish once let go");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success(), "`{backend}` exited {:?}: {said:?}", status.code());
}

/// Grants with every other category open and `listen` as given.
fn listen_grants(listen: &[&str]) -> String {
    grants_module_listening(&Grants { read: &["**"], write: &["**"], network: &["*"] }, listen)
}

/// The port a `listening N` line names, if it names one above zero.
fn chosen(line: &str) -> Option<u16> {
    line.strip_prefix("listening ").and_then(|n| n.parse().ok()).filter(|n| *n > 0)
}

/// **Under `deny` with no `listen` grant, `listen_on` raises `Denied` and
/// binds nothing.** `SocketError::Denied` names `0.0.0.0:<port>`, the
/// address as bound, for a port the program names and for port 0 alike;
/// and while the program is still running, the test binds the same port
/// itself, which it could not do if anything held it.
#[test]
fn listening_without_a_grant_is_denied_and_binds_nothing() {
    let port = free_port();
    let control = TcpListener::bind(("127.0.0.1", 0)).expect("a control port");
    let program = listening(&[port, 0], control.local_addr().expect("an address").port());
    let exe = build_with("perm_listen_denied", Some(listen_grants(&[])), &program);
    for backend in BACKENDS {
        while_held(&exe, backend, &control, 2, |said| {
            assert_eq!(
                said,
                [format!("SocketError::Denied(0.0.0.0:{port})"), "SocketError::Denied(0.0.0.0:0)".to_string()],
                "`{backend}`"
            );
            assert!(can_bind(port), "`{backend}`: a refused listen must leave the port free");
        });
    }
}

/// **A granted port binds, and only that port does.** Under `listen =
/// ["<port>"]` the port is bound -- the test cannot bind it while the
/// program holds it, which is what shows the check above means something --
/// and port 0 is refused: a specific port is not a grant of the one the
/// operating system would choose.
#[test]
fn a_granted_port_binds_and_port_zero_does_not() {
    let port = free_port();
    let control = TcpListener::bind(("127.0.0.1", 0)).expect("a control port");
    let program = listening(&[port, 0], control.local_addr().expect("an address").port());
    let grant = port.to_string();
    let exe = build_with("perm_listen_port", Some(listen_grants(&[&grant])), &program);
    for backend in BACKENDS {
        while_held(&exe, backend, &control, 2, |said| {
            assert_eq!(
                said,
                [format!("listening {port}"), "SocketError::Denied(0.0.0.0:0)".to_string()],
                "`{backend}`"
            );
            assert!(!can_bind(port), "`{backend}`: the granted port should be held");
        });
    }
}

/// **A range covers what is in it.** `low-high` around the port binds it; a
/// range that starts just past it does not.
#[test]
fn a_range_covers_the_ports_in_it() {
    let port = free_port();
    let control = TcpListener::bind(("127.0.0.1", 0)).expect("a control port");
    let program = listening(&[port], control.local_addr().expect("an address").port());
    let around = format!("{}-{}", port - 1, port + 1);
    let past = format!("{}-{}", port + 1, port + 2);
    for (name, grant, expected) in [
        ("perm_listen_range_in", &around, format!("listening {port}")),
        ("perm_listen_range_past", &past, format!("SocketError::Denied(0.0.0.0:{port})")),
    ] {
        let exe = build_with(name, Some(listen_grants(&[grant])), &program);
        for backend in BACKENDS {
            while_held(&exe, backend, &control, 1, |said| {
                assert_eq!(said, std::slice::from_ref(&expected), "`{backend}` under `{grant}`");
            });
        }
    }
}

/// **`0` grants the port the operating system chooses, and nothing else.**
/// Port 0 binds and reads back as a real port; a port the program names is
/// refused. `["0", "<port>"]` grants both.
#[test]
fn zero_grants_an_os_chosen_port() {
    let port = free_port();
    let control = TcpListener::bind(("127.0.0.1", 0)).expect("a control port");
    let program = listening(&[0, port], control.local_addr().expect("an address").port());
    let exe = build_with("perm_listen_zero", Some(listen_grants(&["0"])), &program);
    for backend in BACKENDS {
        while_held(&exe, backend, &control, 2, |said| {
            assert!(chosen(&said[0]).is_some(), "`{backend}`: port 0 should bind: {said:?}");
            assert_eq!(said[1], format!("SocketError::Denied(0.0.0.0:{port})"), "`{backend}`");
        });
    }
    let both = port.to_string();
    let exe = build_with("perm_listen_zero_and_port", Some(listen_grants(&["0", &both])), &program);
    for backend in BACKENDS {
        while_held(&exe, backend, &control, 2, |said| {
            assert!(chosen(&said[0]).is_some(), "`{backend}`: port 0 should bind: {said:?}");
            assert_eq!(said[1], format!("listening {port}"), "`{backend}`");
        });
    }
}

/// Tries `listen_on` on ports no socket can have, printing `failed`, the
/// port it bound, or the refusal.
const OUT_OF_RANGE: &str = r#"module demo::main;

import std::core::{print};
import std::net::socket::{SocketError, listen_on, shut, start};

@LOCAL_PORT@

fn bound(port: Int) -> String raises SocketError {
  let socket = listen_on(port)!;
  if socket < 0 {
    "${Int::to_string(port)} failed"
  } else {
    let said = "${Int::to_string(port)} bound ${Int::to_string(local_port(socket))}";
    shut(socket);
    said
  }
}

pub fn main() -> Int {
  start();
  print(bound(70000)! catch { e => "${e}" });
  print(bound(65536)! catch { e => "${e}" });
  print(bound(-1)! catch { e => "${e}" });
  0
}
"#;

/// **A port outside 0 to 65535 binds nothing, under any grant.** The port
/// goes into a sixteen-bit field, so `70000` bound 4464 and `65536` bound
/// the operating system's choice; under `*`, which covers any address, the
/// program got a port it never asked for. Answered `invalid_handle()`, as a
/// bind the operating system refuses is, so a caller's existing check for a
/// failed listen covers it.
#[test]
fn a_port_outside_the_range_binds_nothing() {
    let program = OUT_OF_RANGE.replace("@LOCAL_PORT@", LOCAL_PORT);
    let exe = build_with("perm_listen_out_of_range", Some(listen_grants(&["*"])), &program);
    for backend in BACKENDS {
        assert_eq!(
            run(&exe, exe.parent().expect("a directory"), backend),
            "70000 failed\n65536 failed\n-1 failed\n",
            "`{backend}`"
        );
    }
}

/// **With no `[permissions]` table, or `default = "allow"`, anything may
/// be bound.** No table compiles with the `grants.kh` on disk; `allow`
/// with no `listen` key writes `*`, which is what this compiles with.
#[test]
fn allow_and_no_table_bind_anything() {
    let port = free_port();
    let control = TcpListener::bind(("127.0.0.1", 0)).expect("a control port");
    let program = listening(&[port, 0], control.local_addr().expect("an address").port());
    for (name, module) in [("perm_listen_no_table", None), ("perm_listen_allow", Some(listen_grants(&["*"])))] {
        let exe = build_with(name, module, &program);
        for backend in BACKENDS {
            while_held(&exe, backend, &control, 2, |said| {
                assert_eq!(said[0], format!("listening {port}"), "`{backend}` ({name})");
                assert!(chosen(&said[1]).is_some(), "`{backend}` ({name}): {said:?}");
            });
        }
    }
}

/// Starts `Router::listen_quietly`, `Router::listen` and
/// `Router::listen_tls` on `PORT` in turn and prints how each ended.
const SERVING: &str = r#"module demo::main;

import std::core::{ChildFailed, SharedFn, print, scoped};
import std::net::http::{HttpError, Response, Router};
import std::net::tls::{TlsError};

fn quietly() -> String raises HttpError + ChildFailed {
  Router::new()
    |> Router::get("/x", SharedFn::of(fn _ => Response::text(200, "ok")))
    |> Router::listen_quietly(@PORT@)!;
  "quietly served"
}

fn loudly() -> String raises HttpError + ChildFailed {
  Router::new()
    |> Router::get("/x", SharedFn::of(fn _ => Response::text(200, "ok")))
    |> Router::listen(@PORT@)!;
  "loudly served"
}

fn secured() -> String raises HttpError + TlsError + ChildFailed {
  scoped(fn () =>
    Router::new()
      |> Router::get("/x", SharedFn::of(fn _ => Response::text(200, "ok")))
      |> Router::listen_tls(@PORT@, @CERT@, @KEY@)!)!;
  "tls served"
}

pub fn main() -> Int {
  print(quietly()! catch { HttpError::Denied(a) => "quietly denied ${a}", _ => "quietly something else" });
  print(loudly()! catch { HttpError::Denied(a) => "loudly denied ${a}", _ => "loudly something else" });
  print(secured()! catch { HttpError::Denied(a) => "tls denied ${a}", _ => "tls something else" });
  0
}
"#;

/// A file in `tests/fixtures`, as a Khora string literal.
fn fixture_literal(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    let text = std::fs::read_to_string(&path).expect("the test certificate is in tests/fixtures");
    format!("{:?}", text.replace('\r', ""))
}

/// **`Router::listen`, `listen_quietly` and a TLS server are refused the
/// same way**, with `HttpError::Denied` naming the address, and nothing is
/// bound: each returns at once rather than serving, and the port is free
/// afterwards.
#[test]
fn a_router_and_a_tls_server_are_refused_alike() {
    let port = free_port();
    let program = SERVING
        .replace("@PORT@", &port.to_string())
        .replace("@CERT@", &fixture_literal("localhost.cert.pem"))
        .replace("@KEY@", &fixture_literal("localhost.key.pem"));
    let exe = build_with("perm_listen_router", Some(listen_grants(&[])), &program);
    for backend in BACKENDS {
        let said = run(&exe, exe.parent().expect("a directory"), backend);
        assert_eq!(
            said,
            format!(
                "quietly denied 0.0.0.0:{port}\nloudly denied 0.0.0.0:{port}\ntls denied 0.0.0.0:{port}\n"
            ),
            "`{backend}`"
        );
        assert!(can_bind(port), "`{backend}`: a refused server must leave the port free");
    }
}
