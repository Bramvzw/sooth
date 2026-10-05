//! The architecture invariants in `AGENTS.md` that clippy has no lint for:
//! which module may depend on which, no dependency cycles, and no file access
//! in `analyzers/`. Read from the source text; test code (from a file's
//! `#[cfg(test)]` on) is not held to the layering.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Dependencies the layering forbids, each tolerated until its issue lands.
const EXCEPTIONS: &[(&str, &str, &str)] = &[];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    Command,
    Presentation,
    Analysis,
    Domain,
    /// `junit.rs`: depends on no other sooth module.
    Parser,
    /// `cli.rs`: the clap vocabulary (`Preset`, `ColorChoice`) other layers name.
    Cli,
}

fn layer(module: &str) -> Layer {
    match module {
        "main" => Layer::Command,
        "report" => Layer::Presentation,
        "junit" => Layer::Parser,
        "cli" => Layer::Cli,
        m if m == "analyzers" || m.starts_with("analyzers::") => Layer::Analysis,
        _ => Layer::Domain,
    }
}

fn may_depend(from: Layer, to: Layer) -> bool {
    use Layer::{Analysis, Cli, Command, Domain, Parser, Presentation};
    match from {
        Command => true,
        Presentation | Analysis => matches!(to, Analysis | Domain | Parser | Cli),
        Domain => matches!(to, Domain | Parser | Cli),
        Parser | Cli => false,
    }
}

/// Every module under `src/` with its non-test source, comments removed.
fn modules() -> BTreeMap<String, String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    collect(&src, &src, &mut found);
    found
}

fn collect(root: &Path, dir: &Path, found: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).expect("src/ is readable") {
        let path = entry.expect("src/ entry is readable").path();
        if path.is_dir() {
            collect(root, &path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let relative = path.strip_prefix(root).expect("path is under src/");
            let module = relative
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "::");
            let text = fs::read_to_string(&path).expect("source file is readable");
            found.insert(module, production(&text));
        }
    }
}

fn production(text: &str) -> String {
    // The test module itself, not any `#[cfg(test)]`: a test-only item
    // mid-file must not hide the production code below it.
    let code = text.find("\nmod tests {").map_or(text, |at| &text[..at]);
    code.lines()
        .map(|line| line.split("//").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The paths named after each `prefix` (`crate::`, `super::`, `std::`),
/// with `use` groups expanded: `crate::{a, b::c}` gives `[a]` and `[b, c]`.
fn paths_after(src: &str, prefix: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = src.chars().collect();
    let needle: Vec<char> = prefix.chars().collect();
    let mut paths = Vec::new();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        let preceded_by_ident = i > 0 && is_ident(chars[i - 1]);
        if chars[i..i + needle.len()] == needle[..] && !preceded_by_ident {
            i += needle.len();
            paths.extend(tree(&chars, &mut i));
        } else {
            i += 1;
        }
    }
    paths
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn tree(s: &[char], i: &mut usize) -> Vec<Vec<String>> {
    skip_whitespace(s, i);
    match s.get(*i) {
        Some('{') => {
            *i += 1;
            let mut paths = Vec::new();
            loop {
                skip_whitespace(s, i);
                match s.get(*i) {
                    None => return paths,
                    Some('}') => {
                        *i += 1;
                        return paths;
                    }
                    _ => paths.extend(tree(s, i)),
                }
                while s.get(*i).is_some_and(|c| *c != ',' && *c != '}') {
                    *i += 1;
                }
                if s.get(*i) == Some(&',') {
                    *i += 1;
                }
            }
        }
        Some('*') => {
            *i += 1;
            vec![Vec::new()]
        }
        _ => {
            let start = *i;
            while s.get(*i).is_some_and(|c| is_ident(*c)) {
                *i += 1;
            }
            let ident: String = s[start..*i].iter().collect();
            if ident.is_empty() {
                return vec![Vec::new()];
            }
            if s.get(*i..*i + 2) == Some(&[':', ':'][..]) {
                *i += 2;
                tree(s, i)
                    .into_iter()
                    .map(|mut path| {
                        path.insert(0, ident.clone());
                        path
                    })
                    .collect()
            } else {
                vec![vec![ident]]
            }
        }
    }
}

fn skip_whitespace(s: &[char], i: &mut usize) {
    while s.get(*i).is_some_and(|c| c.is_whitespace()) {
        *i += 1;
    }
}

/// The module an absolute path lands in: its longest prefix that is a
/// module, or `main` for an item of the crate root.
fn owner(path: &[String], known: &BTreeMap<String, String>) -> String {
    (1..=path.len())
        .rev()
        .map(|n| path[..n].join("::"))
        .find(|candidate| known.contains_key(candidate))
        .unwrap_or_else(|| "main".to_string())
}

/// `super::…` read from `module`, as an absolute path. Assumes the path is
/// not inside an inline `mod` block of that file.
fn absolute(module: &str, relative: &[String]) -> Vec<String> {
    let mut base: Vec<String> = module.split("::").map(String::from).collect();
    base.pop();
    let mut rest = relative;
    while rest.first().is_some_and(|s| s == "super") {
        base.pop();
        rest = &rest[1..];
    }
    base.extend(rest.iter().cloned());
    base
}

/// Every module-to-module dependency. `main.rs` is the crate root and names
/// modules bare, so its edges are not read; the command layer may use all.
/// A parent module depends on its children (`analyzers` on `analyzers::flaky`),
/// so a cycle through `crate::analyzers` is seen; a child that uses its
/// parent's own items would therefore show as a cycle.
fn edges(known: &BTreeMap<String, String>) -> BTreeSet<(String, String)> {
    let mut edges = BTreeSet::new();
    for (module, src) in known.iter().filter(|(m, _)| *m != "main") {
        let crate_paths = paths_after(src, "crate::");
        let super_paths = paths_after(src, "super::")
            .into_iter()
            .map(|p| absolute(module, &p));
        for path in crate_paths.into_iter().chain(super_paths) {
            let target = owner(&path, known);
            if target != *module {
                edges.insert((module.clone(), target));
            }
        }
        if let Some((parent, _)) = module.rsplit_once("::") {
            edges.insert((parent.to_string(), module.clone()));
        }
    }
    edges
}

fn is_exception(from: &str, to: &str) -> bool {
    EXCEPTIONS.iter().any(|(f, t, _)| *f == from && *t == to)
}

#[test]
fn every_dependency_points_down_the_layers() {
    let known = modules();
    let violations: Vec<String> = edges(&known)
        .into_iter()
        .filter(|(from, to)| !may_depend(layer(from), layer(to)) && !is_exception(from, to))
        .map(|(from, to)| {
            format!(
                "{from} ({:?}) depends on {to} ({:?})",
                layer(&from),
                layer(&to)
            )
        })
        .collect();
    assert!(
        violations.is_empty(),
        "dependencies against the layering in AGENTS.md:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn no_dependency_cycle_exists() {
    let known = modules();
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (from, to) in edges(&known) {
        if !is_exception(&from, &to) {
            graph.entry(from).or_default().push(to);
        }
    }
    let mut done = BTreeSet::new();
    for start in graph.keys() {
        if let Some(cycle) = find_cycle(&graph, start, &mut Vec::new(), &mut done) {
            panic!("dependency cycle (AGENTS.md): {}", cycle.join(" -> "));
        }
    }
}

fn find_cycle(
    graph: &BTreeMap<String, Vec<String>>,
    node: &str,
    path: &mut Vec<String>,
    done: &mut BTreeSet<String>,
) -> Option<Vec<String>> {
    if let Some(at) = path.iter().position(|n| n == node) {
        let mut cycle = path[at..].to_vec();
        cycle.push(node.to_string());
        return Some(cycle);
    }
    if done.contains(node) {
        return None;
    }
    path.push(node.to_string());
    for next in graph.get(node).into_iter().flatten() {
        if let Some(cycle) = find_cycle(graph, next, path, done) {
            return Some(cycle);
        }
    }
    path.pop();
    done.insert(node.to_string());
    None
}

#[test]
fn every_exception_is_still_needed() {
    let edges = edges(&modules());
    for (from, to, issue) in EXCEPTIONS {
        assert!(
            edges.contains(&((*from).to_string(), (*to).to_string())),
            "{from} no longer depends on {to}: remove the exception ({issue}) here and in AGENTS.md"
        );
    }
}

#[test]
fn analyzers_reach_for_no_io_module() {
    for (module, src) in modules() {
        if layer(&module) == Layer::Analysis {
            for path in paths_after(&src, "std::") {
                if let Some(io @ ("fs" | "io" | "net" | "env")) = path.first().map(String::as_str) {
                    panic!("{module} uses std::{io}: analyzers do no I/O (AGENTS.md)");
                }
            }
        }
    }
}

#[test]
fn a_use_group_names_each_of_its_paths() {
    let paths = paths_after(
        "use crate::{junit, analyzers::{flaky, history as h}, report::*};",
        "crate::",
    );
    let expected: Vec<Vec<String>> = [
        vec!["junit"],
        vec!["analyzers", "flaky"],
        vec!["analyzers", "history"],
        vec!["report"],
    ]
    .iter()
    .map(|p| p.iter().map(|s| (*s).to_string()).collect())
    .collect();
    assert_eq!(paths, expected);
}

#[test]
fn a_path_mentioned_in_a_comment_or_test_is_not_a_dependency() {
    let src = production(
        "use crate::junit; // not crate::report\n#[cfg(test)]\nmod tests {\n    use crate::main;\n}",
    );
    assert_eq!(
        paths_after(&src, "crate::"),
        vec![vec!["junit".to_string()]]
    );
}

#[test]
fn super_resolves_from_the_file_module() {
    let path = |s: &str| s.split("::").map(String::from).collect::<Vec<_>>();
    assert_eq!(
        absolute("analyzers::flaky", &path("history")),
        path("analyzers::history")
    );
    assert_eq!(
        absolute("analyzers::flaky", &path("super::junit")),
        path("junit")
    );
}
