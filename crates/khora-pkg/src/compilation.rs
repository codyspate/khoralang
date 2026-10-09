//! The source files a consumer compiles from each resolved dependency.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use khora_db::{KhoraDatabase, SourceFile};

use crate::{resolve, resolve_cached, Resolution, Store};

/// Whether a compilation may update its lockfile and fetch packages.
#[derive(Debug, Clone, Copy)]
pub enum SourceMode {
    /// The command-line build's existing resolution policy.
    Build {
        /// A CI build refuses lockfile updates but may still populate the store.
        locked: bool,
    },
    /// Refuse a missing lock entry or store directory without network access.
    Cached,
}

/// A resolved graph and precisely the dependency files a consumer compiles.
#[derive(Debug)]
pub struct Compilation {
    /// The graph, needed by callers enforcing package permissions.
    pub resolution: Resolution,
    /// Files under the published module tree of each dependency.
    pub files: Vec<PathBuf>,
}

/// Resolve a manifest and select each dependency's published module tree.
///
/// Keeping selection here prevents an editor from exposing a dependency's
/// private test modules while a build correctly excludes them. The walk pays
/// for reading each source's module header, as the CLI did previously.
pub fn compilation(manifest: &Path, store: &Store, mode: SourceMode) -> Result<Compilation> {
    let resolution = match mode {
        SourceMode::Build { locked } => resolve(manifest, store, locked)?,
        SourceMode::Cached => resolve_cached(manifest, store)?,
    };
    let mut files = Vec::new();
    for (name, directory) in resolution.named_directories() {
        let mut theirs = Vec::new();
        walk(&directory, &mut theirs)?;
        theirs.retain(|file| module_belongs_to(module_of(file).as_deref(), &name));
        files.extend(theirs);
    }
    Ok(Compilation { resolution, files })
}

fn module_belongs_to(module: Option<&str>, package: &str) -> bool {
    let Some(module) = module else { return true };
    let normalize = |path: &str| path.replace("::", ".");
    let owner = normalize(package);
    let module = normalize(module);
    module == owner || module.starts_with(&format!("{owner}."))
}

fn module_of(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, path.to_path_buf(), text);
    khora_hir::item_map(&db, file)
        .module
        .as_ref()
        .map(ToString::to_string)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "build" || n == "target" || n == ".git")
            {
                continue;
            }
            if path.join("khora.toml").is_file() || is_bin_dir(&path) {
                continue;
            }
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kh")
            && khora_db::selected_for_target(&path, khora_db::host_target())
        {
            out.push(path);
        }
    }
    Ok(())
}

fn is_bin_dir(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == "bin")
        && dir
            .parent()
            .is_some_and(|src| src.file_name().is_some_and(|n| n == "src"))
        && dir
            .parent()
            .and_then(Path::parent)
            .is_some_and(|root| root.join("khora.toml").is_file())
}
