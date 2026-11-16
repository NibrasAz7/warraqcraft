//! WarraqCraft package catalog (L3).
//!
//! A statically compiled catalog of common LaTeX commands and
//! environments — WarraqCraft's own completion data, authored from the
//! LaTeX2e documentation and CTAN package manuals (never copied from
//! other editors' `.cwl` files). It powers command, environment and
//! package completion in the editor UI, and doubles as the seed of the
//! package/class registry that will load WarraqCraft's bundled packages
//! (like `arabdoc`) later.
//!
//! The data is pure `&'static` tables — zero dependencies, zero
//! allocation at lookup time except for [`packages`], which returns a
//! freshly deduplicated list.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod catalog;
mod registry;

pub use catalog::{ArgKind, ArgSpec, CommandSpec};
pub use registry::{registry, resolve, search, PackageInfo};

/// The full catalog, sorted by [`CommandSpec::name`].
pub fn catalog() -> &'static [CommandSpec] {
    catalog::CATALOG
}

/// Commands and environments whose `name` starts with `prefix`
/// (case-sensitive). Because the table is sorted, matches are yielded
/// in name order.
pub fn find_prefix(prefix: &str) -> impl Iterator<Item = &'static CommandSpec> + '_ {
    let all = catalog();
    let start = all.partition_point(|c| c.name < prefix);
    all.get(start..).into_iter().flatten().take_while(move |c| c.name.starts_with(prefix))
}

/// Exact lookup by `name` (case-sensitive).
pub fn find_exact(name: &str) -> Option<&'static CommandSpec> {
    catalog().binary_search_by(|c| c.name.cmp(name)).ok().and_then(|i| catalog().get(i))
}

/// Distinct package names present in the catalog, sorted.
pub fn packages() -> impl Iterator<Item = &'static str> {
    let mut pkgs: Vec<&'static str> = catalog().iter().map(|c| c.package).collect();
    pkgs.sort_unstable();
    pkgs.dedup();
    pkgs.into_iter()
}

/// The catalog entries that are environments (`\begin{name}`…`\end{name}`).
pub fn environments() -> impl Iterator<Item = &'static CommandSpec> {
    catalog().iter().filter(|c| c.is_env)
}
