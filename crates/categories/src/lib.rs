//! File categories (games, videos, caches, ...) for Bytescape scan trees.
//!
//! Categories and their rules come from TOML: the built-in
//! [`DEFAULT_RULES`], then optionally a user rules file loaded after it.
//! See `rules/default.toml` for the format.
//!
//! How a node gets its category:
//!
//! 1. Path rules: globs matched against the node's full path (`/`
//!    separators, case ignored). A trailing `/**` makes a folder rule that
//!    covers the folder and everything under it. A folder's match is
//!    inherited by everything below it, so the deepest matching folder wins.
//!    A rule starting with `!` clears the path category instead (an
//!    exception to a broader rule), so extension rules apply there again.
//! 2. Extension rules, for files with no path match. The longest matching
//!    extension wins (`tar.gz` before `gz`).
//! 3. The fallback category, for files nothing matched.
//!
//! On a tie (several rules match the same node), the rule loaded last wins,
//! so user rules beat the defaults. Folders only get path categories; a
//! folder no path rule covers has category [`NONE`].
//!
//! ```
//! let rules = categories::Rules::defaults();
//! let id = rules.classify("C:/Windows/Temp/x.log", false);
//! assert_eq!(rules.category(id).unwrap().id, "caches");
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use scanner::{flags, NodeId, NodeKind, ScanTree};
use serde::Deserialize;

/// The built-in rules file.
pub const DEFAULT_RULES: &str = include_str!("../rules/default.toml");

/// Numeric category id, as stored in `scanner::Node::category`. Categories
/// are numbered from 1 in the order they are first defined.
pub type CategoryId = u16;

/// No category.
pub const NONE: CategoryId = 0;

/// How safe it usually is to delete files in a category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cleanup {
    /// Warn before deleting.
    Never,
    /// Look before deleting.
    Review,
    /// Usually safe to clean.
    Safe,
}

/// One category, as defined by the rules files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    /// Stable text id from the rules file, for example `"games"`.
    pub id: String,
    pub name: String,
    /// Colour name for the UI, for example `"violet"`.
    pub color: String,
    pub cleanup: Cleanup,
}

/// A rules file that could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesError {
    /// Name of the rules file, as passed to [`Rules::load`].
    pub file: String,
    pub message: String,
}

impl fmt::Display for RulesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.file, self.message)
    }
}

impl std::error::Error for RulesError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    fallback: Option<String>,
    #[serde(default)]
    category: Vec<RawCategory>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCategory {
    id: String,
    name: Option<String>,
    color: Option<String>,
    cleanup: Option<Cleanup>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
}

/// Compiled category rules.
#[derive(Debug)]
pub struct Rules {
    categories: Vec<Category>,
    /// All path globs in load order; the index is the tie-break priority.
    globs: GlobSet,
    /// Per glob: its category, and whether it only matches folders.
    glob_info: Vec<(CategoryId, bool)>,
    /// Whether any glob can match a file (otherwise file paths are not built).
    file_globs: bool,
    /// Lower-case extension (without the dot) to category.
    extensions: HashMap<String, CategoryId>,
    fallback: CategoryId,
}

impl Rules {
    /// The built-in rules.
    pub fn defaults() -> Rules {
        Rules::load(&[("default rules", DEFAULT_RULES)]).expect("built-in rules are valid")
    }

    /// The built-in rules, then a user rules file on top.
    pub fn with_user_rules(file: &str, text: &str) -> Result<Rules, RulesError> {
        Rules::load(&[("default rules", DEFAULT_RULES), (file, text)])
    }

    /// Loads rules files in order, as `(file name, TOML text)` pairs. Later
    /// files add to and override earlier ones.
    pub fn load(files: &[(&str, &str)]) -> Result<Rules, RulesError> {
        let mut categories: Vec<Category> = Vec::new();
        let mut globs = GlobSetBuilder::new();
        // Per glob, in load order: its category and whether it only matches folders.
        let mut glob_info: Vec<(CategoryId, bool)> = Vec::new();
        let mut extensions = HashMap::new();
        let mut fallback: Option<(String, &str)> = None;

        for &(file, text) in files {
            let err = |message: String| RulesError {
                file: file.to_string(),
                message,
            };
            let raw: RawFile = toml::from_str(text).map_err(|e| err(e.to_string()))?;
            if let Some(f) = raw.fallback {
                fallback = Some((f, file));
            }
            for rc in raw.category {
                let id = match categories.iter().position(|c| c.id == rc.id) {
                    Some(i) => {
                        let c = &mut categories[i];
                        c.name = rc.name.unwrap_or(std::mem::take(&mut c.name));
                        c.color = rc.color.unwrap_or(std::mem::take(&mut c.color));
                        c.cleanup = rc.cleanup.unwrap_or(c.cleanup);
                        i + 1
                    }
                    None => {
                        if rc.id.is_empty() {
                            return Err(err("a category has an empty id".into()));
                        }
                        categories.push(Category {
                            name: rc.name.unwrap_or_else(|| rc.id.clone()),
                            id: rc.id,
                            color: rc.color.unwrap_or_else(|| "gray".into()),
                            cleanup: rc.cleanup.unwrap_or(Cleanup::Review),
                        });
                        categories.len()
                    }
                };
                let id = CategoryId::try_from(id).map_err(|_| err("too many categories".into()))?;
                for p in rc.paths {
                    let (p, cat) = match p.strip_prefix('!') {
                        Some(rest) => (rest.replace('\\', "/"), NONE),
                        None => (p.replace('\\', "/"), id),
                    };
                    let (pattern, dir_only) = match p.strip_suffix("/**") {
                        Some(dir) if !dir.is_empty() => (dir.to_string(), true),
                        _ => (p, false),
                    };
                    let glob = GlobBuilder::new(&pattern)
                        .case_insensitive(true)
                        .literal_separator(true)
                        .backslash_escape(false)
                        .build()
                        .map_err(|e| {
                            let owner = &categories[id as usize - 1].id;
                            err(format!("category {owner:?}: {e}"))
                        })?;
                    globs.add(glob);
                    glob_info.push((cat, dir_only));
                }
                for e in rc.extensions {
                    let e = e.trim_start_matches('.').to_lowercase();
                    if e.is_empty() {
                        return Err(err(format!(
                            "category {:?} has an empty extension",
                            categories[id as usize - 1].id
                        )));
                    }
                    extensions.insert(e, id);
                }
            }
        }

        let fallback = match fallback {
            None => NONE,
            Some((f, file)) => match categories.iter().position(|c| c.id == f) {
                Some(i) => (i + 1) as CategoryId,
                None => {
                    return Err(RulesError {
                        file: file.to_string(),
                        message: format!("fallback {f:?} is not a category"),
                    })
                }
            },
        };

        let globs = globs.build().map_err(|e| RulesError {
            file: "rules".into(),
            message: e.to_string(),
        })?;

        Ok(Rules {
            file_globs: glob_info.iter().any(|&(_, dir_only)| !dir_only),
            glob_info,
            globs,
            categories,
            extensions,
            fallback,
        })
    }

    /// All categories; category id `n` is `categories()[n - 1]`.
    pub fn categories(&self) -> &[Category] {
        &self.categories
    }

    /// The category with this numeric id, or `None` for [`NONE`] or an unknown id.
    pub fn category(&self, id: CategoryId) -> Option<&Category> {
        (id as usize)
            .checked_sub(1)
            .and_then(|i| self.categories.get(i))
    }

    /// Numeric id of the category with this text id.
    pub fn id_of(&self, id: &str) -> Option<CategoryId> {
        self.categories
            .iter()
            .position(|c| c.id == id)
            .map(|i| (i + 1) as CategoryId)
    }

    /// The category files fall back to when nothing matches.
    pub fn fallback(&self) -> CategoryId {
        self.fallback
    }

    /// Category of one path, checking every folder above it. `path` may use
    /// `\` or `/`. For whole trees use [`Rules::apply`], which matches each
    /// folder once.
    pub fn classify(&self, path: &str, is_dir: bool) -> CategoryId {
        let path = normalize(path);
        let mut inherited = NONE;
        for (i, _) in path.match_indices('/') {
            let dir = &path[..i];
            if !dir.is_empty() && !dir.ends_with('/') {
                inherited = self.match_path(dir, true).unwrap_or(inherited);
            }
        }
        let name = path.rsplit('/').next().unwrap_or(&path);
        let kind = if is_dir {
            NodeKind::Dir
        } else {
            NodeKind::File
        };
        self.node_category(Some(&path), name, kind, inherited)
    }

    /// Sets every node's category in a scanned tree.
    pub fn apply(&self, tree: &mut ScanTree) {
        if tree.is_empty() {
            return;
        }
        // Folder paths, kept while their children are visited. Parents
        // always come before their children in the arena.
        let mut dir_paths: Vec<Box<str>> = vec![Box::from(""); tree.len()];
        let mut file_path = String::new();
        for i in 0..tree.len() {
            let id = i as NodeId;
            let node = tree.node(id);
            let cat = if id == tree.root() {
                // The folders above the scanned one count too: scanning a
                // folder inside node_modules is still developer dependencies.
                let path = normalize(&node.name);
                let cat = self.classify(&path, node.kind == NodeKind::Dir);
                dir_paths[i] = path.into();
                cat
            } else {
                let parent = &dir_paths[node.parent as usize];
                let inherited = tree.node(node.parent).category;
                if node.kind == NodeKind::Dir {
                    let path = join(parent, &node.name);
                    let cat = self.node_category(Some(&path), &node.name, node.kind, inherited);
                    dir_paths[i] = path.into();
                    cat
                } else if self.file_globs {
                    file_path.clear();
                    file_path.push_str(parent);
                    if !parent.ends_with('/') {
                        file_path.push('/');
                    }
                    file_path.push_str(&node.name);
                    self.node_category(Some(&file_path), &node.name, node.kind, inherited)
                } else {
                    self.node_category(None, &node.name, node.kind, inherited)
                }
            };
            tree.set_category(id, cat);
        }
    }

    /// Per-category file totals of an already categorised tree, indexed by
    /// category id (index 0 is [`NONE`]). Duplicate hardlinks are skipped.
    pub fn totals(&self, tree: &ScanTree) -> Vec<CategoryTotal> {
        let mut out: Vec<CategoryTotal> = (0..=self.categories.len())
            .map(|i| CategoryTotal {
                category: i as CategoryId,
                ..Default::default()
            })
            .collect();
        for n in tree.nodes() {
            if n.kind != NodeKind::File || n.flags & (flags::HARDLINK_DUP | flags::REMOVED) != 0 {
                continue;
            }
            if let Some(t) = out.get_mut(n.category as usize) {
                t.files += 1;
                t.size += n.size;
                t.alloc += n.alloc;
            }
        }
        out
    }

    /// Category of one node, given the category its parent folder passes down.
    fn node_category(
        &self,
        path: Option<&str>,
        name: &str,
        kind: NodeKind,
        inherited: CategoryId,
    ) -> CategoryId {
        let is_dir = kind == NodeKind::Dir;
        // A matching `!` rule gives `Some(NONE)`, which overrides `inherited`.
        let path_cat = path
            .and_then(|p| self.match_path(p, is_dir))
            .unwrap_or(inherited);
        if path_cat != NONE || kind != NodeKind::File {
            return path_cat;
        }
        self.match_extension(name).unwrap_or(self.fallback)
    }

    /// The highest-priority path rule matching `path`, if any.
    fn match_path(&self, path: &str, is_dir: bool) -> Option<CategoryId> {
        thread_local! {
            static HITS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        }
        HITS.with_borrow_mut(|hits| {
            self.globs.matches_into(path, hits);
            hits.iter()
                .filter(|&&i| is_dir || !self.glob_info[i].1)
                .max()
                .map(|&i| self.glob_info[i].0)
        })
    }

    /// Category of the longest extension of `name` that has a rule.
    fn match_extension(&self, name: &str) -> Option<CategoryId> {
        // Leading dots (".gitignore") do not start an extension.
        let body = name.trim_start_matches('.');
        let offset = name.len() - body.len();
        for (i, _) in body.match_indices('.') {
            let ext = &name[offset + i + 1..];
            if ext.is_empty() {
                continue;
            }
            let hit = if ext.bytes().any(|b| b.is_ascii_uppercase()) || !ext.is_ascii() {
                self.extensions.get(&ext.to_lowercase())
            } else {
                self.extensions.get(ext)
            };
            if let Some(&c) = hit {
                return Some(c);
            }
        }
        None
    }
}

/// Files and bytes in one category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CategoryTotal {
    pub category: CategoryId,
    pub files: u64,
    pub size: u64,
    pub alloc: u64,
}

/// `/` separators, without Windows' `\\?\` prefix.
fn normalize(path: &str) -> String {
    let p = path.replace('\\', "/");
    if let Some(rest) = p.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = p.strip_prefix("//?/") {
        rest.to_string()
    } else {
        p
    }
}

fn join(dir: &str, name: &str) -> String {
    let mut s = String::with_capacity(dir.len() + 1 + name.len());
    s.push_str(dir);
    if !dir.ends_with('/') {
        s.push('/');
    }
    s.push_str(name);
    s
}
