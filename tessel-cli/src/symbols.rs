//! Symbol extraction with tree-sitter: which functions, methods and types a source file defines,
//! where each one starts and ends, and where its signature stops and its body begins.
//!
//! The extractor never guesses. A path in a language it does not know gives `None`, and so does
//! a file with any syntax error: the callers then claim and report the whole file.

use std::ops::Range;

use tree_sitter::{Node, Parser};

/// A language with a grammar in this binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
}

/// One named definition. Containers (`mod`, `impl`, `trait`, `class`, `namespace`) are not
/// symbols; their members are, named after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The `SymbolId` name: `auth::session::Session::refresh` for Rust, `Session.refresh` for
    /// TypeScript.
    pub name: String,
    /// The whole definition.
    pub range: Range<usize>,
    /// Everything before the body. A definition with no body (a struct, a constant, a trait
    /// method without a default) is all signature, so any change to it is a signature change.
    pub signature: Range<usize>,
}

impl Symbol {
    pub fn body(&self) -> Range<usize> {
        self.signature.end..self.range.end
    }
}

pub fn language_of(path: &str) -> Option<Language> {
    let (_, extension) = path.rsplit_once('.')?;
    match extension {
        "rs" => Some(Language::Rust),
        "ts" | "mts" | "cts" => Some(Language::TypeScript),
        "tsx" => Some(Language::Tsx),
        _ => None,
    }
}

/// The symbols of `source`, in source order, or `None` when `path` is in a language without a
/// grammar here or `source` has a syntax error anywhere.
pub fn extract(path: &str, source: &str) -> Option<Vec<Symbol>> {
    let language = language_of(path)?;
    let mut parser = Parser::new();
    let grammar = match language {
        Language::Rust => tree_sitter_rust::LANGUAGE,
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
    };
    parser.set_language(&grammar.into()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut out = Vec::new();
    match language {
        Language::Rust => rust_items(root, source, &rust_module_path(path), &mut out),
        Language::TypeScript | Language::Tsx => ts_items(root, source, "", &mut out),
    }
    if out.iter().any(|symbol| !is_canonical_name(&symbol.name)) {
        return None;
    }
    Some(out)
}

/// Whether the coordinator would accept `name` in a `SymbolId`. A copy of `is_canonical_name` and
/// `is_display_hazard` in `src/coordinator.rs`, which refuses any other name as malformed: keep
/// the two in step. A file with a name that fails is claimed whole instead.
fn is_canonical_name(name: &str) -> bool {
    !name.is_empty() && name.trim() == name && !name.chars().any(is_display_hazard)
}

fn is_display_hazard(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{61c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

// ---------- shared helpers ----------

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field).map(|n| text(n, source))
}

fn join(prefix: &str, separator: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}{separator}{name}")
    }
}

/// A symbol whose signature ends where `body` starts.
fn with_body(name: String, outer: Node<'_>, body: Node<'_>) -> Symbol {
    Symbol {
        name,
        range: outer.byte_range(),
        signature: outer.start_byte()..body.start_byte(),
    }
}

/// A symbol that is all signature.
fn whole(name: String, outer: Node<'_>) -> Symbol {
    Symbol {
        name,
        range: outer.byte_range(),
        signature: outer.byte_range(),
    }
}

// ---------- Rust ----------

/// `src/auth/session.rs` is `auth::session`; `src/auth/mod.rs` is `auth`; `src/lib.rs` is the
/// crate root. Anything before the first `src` directory is the crate's own directory.
fn rust_module_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let after_src = parts
        .iter()
        .position(|part| *part == "src")
        .map_or(&parts[..], |at| &parts[at + 1..]);
    let mut names: Vec<&str> = after_src.to_vec();
    if let Some(last) = names.pop() {
        let stem = last.strip_suffix(".rs").unwrap_or(last);
        if !matches!(stem, "mod" | "lib" | "main") {
            names.push(stem);
        }
    }
    names.join("::")
}

fn rust_items(container: Node<'_>, source: &str, prefix: &str, out: &mut Vec<Symbol>) {
    let mut cursor = container.walk();
    for item in container.named_children(&mut cursor) {
        match item.kind() {
            "function_item" => {
                if let (Some(name), Some(body)) = (
                    field_text(item, "name", source),
                    item.child_by_field_name("body"),
                ) {
                    out.push(with_body(join(prefix, "::", name), item, body));
                }
            }
            "function_signature_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "macro_definition" => {
                if let Some(name) = field_text(item, "name", source) {
                    out.push(whole(join(prefix, "::", name), item));
                }
            }
            "mod_item" | "trait_item" => {
                if let (Some(name), Some(body)) = (
                    field_text(item, "name", source),
                    item.child_by_field_name("body"),
                ) {
                    rust_items(body, source, &join(prefix, "::", name), out);
                }
            }
            "impl_item" => {
                if let (Some(label), Some(body)) = (
                    rust_impl_label(item, source),
                    item.child_by_field_name("body"),
                ) {
                    rust_items(body, source, &join(prefix, "::", &label), out);
                }
            }
            _ => {}
        }
    }
}

/// `Session` for `impl<T> Session<T>`, and `<Session as Refresh>` for a trait impl, so a trait
/// method never takes the id of an inherent method of the same name.
fn rust_impl_label(item: Node<'_>, source: &str) -> Option<String> {
    let target = rust_type_name(item.child_by_field_name("type")?, source);
    match item.child_by_field_name("trait") {
        Some(tr) => Some(format!("<{target} as {}>", rust_type_name(tr, source))),
        None => Some(target),
    }
}

/// The type's name without generic arguments, comments removed, whitespace collapsed.
fn rust_type_name(node: Node<'_>, source: &str) -> String {
    let base = match (node.kind(), node.child_by_field_name("type")) {
        ("generic_type", Some(inner)) => inner,
        _ => node,
    };
    let mut bare = String::new();
    let mut at = base.start_byte();
    without_comments(base, source, &mut at, &mut bare);
    bare.push_str(&source[at..base.end_byte()]);
    bare.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Appends the text of `node` up to its comments, each of which becomes a space.
fn without_comments(node: Node<'_>, source: &str, at: &mut usize, out: &mut String) {
    if node.kind().ends_with("comment") {
        out.push_str(&source[*at..node.start_byte()]);
        out.push(' ');
        *at = node.end_byte();
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        without_comments(child, source, at, out);
    }
}

// ---------- TypeScript and TSX ----------

fn ts_items(container: Node<'_>, source: &str, prefix: &str, out: &mut Vec<Symbol>) {
    let mut cursor = container.walk();
    for statement in container.named_children(&mut cursor) {
        let item = match statement.kind() {
            "export_statement" => statement.child_by_field_name("declaration"),
            "ambient_declaration" | "expression_statement" => statement.named_child(0),
            _ => Some(statement),
        };
        if let Some(item) = item {
            ts_item(item, statement, source, prefix, out);
        }
    }
}

/// `item` is the declaration; `outer` is the node whose range the symbol takes, which differs
/// when `export` or `declare` wraps the declaration.
fn ts_item(item: Node<'_>, outer: Node<'_>, source: &str, prefix: &str, out: &mut Vec<Symbol>) {
    match item.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let (Some(name), Some(body)) = (
                field_text(item, "name", source),
                item.child_by_field_name("body"),
            ) {
                out.push(with_body(join(prefix, ".", name), outer, body));
            }
        }
        "function_signature"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration" => {
            if let Some(name) = field_text(item, "name", source) {
                out.push(whole(join(prefix, ".", name), outer));
            }
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let (Some(name), Some(body)) = (
                field_text(item, "name", source),
                item.child_by_field_name("body"),
            ) {
                ts_members(body, source, &join(prefix, ".", name), out);
            }
        }
        "internal_module" => {
            if let (Some(name), Some(body)) = (
                field_text(item, "name", source),
                item.child_by_field_name("body"),
            ) {
                ts_items(body, source, &join(prefix, ".", name), out);
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            ts_variable(item, outer, source, prefix, out);
        }
        _ => {}
    }
}

/// `const f = (a) => { .. }` is a function named `f`; any other single `const x = ..` is a
/// definition that is all signature. A declaration of several names is nobody's symbol.
fn ts_variable(item: Node<'_>, outer: Node<'_>, source: &str, prefix: &str, out: &mut Vec<Symbol>) {
    let mut cursor = item.walk();
    let declarators: Vec<Node<'_>> = item
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "variable_declarator")
        .collect();
    let [declarator] = declarators.as_slice() else {
        return;
    };
    let Some(name) = field_text(*declarator, "name", source) else {
        return;
    };
    let name = join(prefix, ".", name);
    match ts_function_body(declarator.child_by_field_name("value")) {
        Some(body) => out.push(with_body(name, outer, body)),
        None => out.push(whole(name, outer)),
    }
}

/// The body of an arrow function or function expression, if `value` is one.
fn ts_function_body(value: Option<Node<'_>>) -> Option<Node<'_>> {
    let value = value?;
    match value.kind() {
        "arrow_function" | "function_expression" | "function" | "generator_function" => {
            value.child_by_field_name("body")
        }
        _ => None,
    }
}

fn ts_members(body: Node<'_>, source: &str, prefix: &str, out: &mut Vec<Symbol>) {
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        let Some(name) = field_text(member, "name", source) else {
            continue;
        };
        let name = join(prefix, ".", name);
        match member.kind() {
            "method_definition" => {
                if let Some(body) = member.child_by_field_name("body") {
                    out.push(with_body(name, member, body));
                }
            }
            "method_signature" | "abstract_method_signature" => out.push(whole(name, member)),
            "public_field_definition" => {
                match ts_function_body(member.child_by_field_name("value")) {
                    Some(body) => out.push(with_body(name, member, body)),
                    None => out.push(whole(name, member)),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(path: &str, source: &str) -> Vec<String> {
        let symbols = extract(path, source).unwrap();
        symbols.into_iter().map(|s| s.name).collect()
    }

    fn find(path: &str, source: &str, name: &str) -> Symbol {
        let symbols = extract(path, source).unwrap();
        let found = symbols.into_iter().find(|s| s.name == name);
        found.unwrap()
    }

    const RUST_NESTED: &str = "\
use std::fmt;

pub fn top() {}

pub mod inner {
    pub fn helper() {}

    pub mod deep {
        pub struct Thing {
            pub a: u32,
        }
    }
}
";

    #[test]
    fn rust_module_path_comes_from_the_file_and_from_nested_mods() {
        assert_eq!(
            names("src/auth/session.rs", RUST_NESTED),
            [
                "auth::session::top",
                "auth::session::inner::helper",
                "auth::session::inner::deep::Thing",
            ]
        );
        assert_eq!(names("src/lib.rs", RUST_NESTED)[0], "top");
        assert_eq!(
            names("tessel-cli/src/auth/mod.rs", RUST_NESTED)[0],
            "auth::top"
        );
        assert_eq!(names("tests/cli.rs", RUST_NESTED)[0], "tests::cli::top");
    }

    const RUST_IMPLS: &str = "\
pub struct Session;

impl Session {
    pub fn refresh(&self) -> bool {
        true
    }
    const LIMIT: u32 = 3;
}

impl<T: Clone> Wrapper<T> {
    fn get(&self) {}
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

pub trait Refresh {
    fn refresh(&self);
    fn twice(&self) {
        self.refresh();
    }
}
";

    #[test]
    fn rust_impls_trait_impls_and_traits_name_their_members() {
        assert_eq!(
            names("src/auth/session.rs", RUST_IMPLS),
            [
                "auth::session::Session",
                "auth::session::Session::refresh",
                "auth::session::Session::LIMIT",
                "auth::session::Wrapper::get",
                "auth::session::<Session as fmt::Display>::fmt",
                "auth::session::Refresh::refresh",
                "auth::session::Refresh::twice",
            ]
        );
    }

    #[test]
    fn a_rust_function_signature_stops_where_the_body_starts() {
        let source =
            "impl Session {\n    pub fn refresh(&self) -> bool {\n        true\n    }\n}\n";
        let symbol = find("src/s.rs", source, "s::Session::refresh");
        assert_eq!(
            &source[symbol.signature.clone()],
            "pub fn refresh(&self) -> bool "
        );
        assert_eq!(&source[symbol.body()], "{\n        true\n    }");
    }

    #[test]
    fn a_definition_without_a_body_is_all_signature() {
        let source =
            "pub struct A {\n    x: u32,\n}\nconst N: u32 = 1;\ntrait T {\n    fn m(&self);\n}\n";
        for name in ["A", "N", "T::m"] {
            let symbol = find("src/lib.rs", source, name);
            assert_eq!(symbol.signature, symbol.range, "{name}");
            assert!(symbol.body().is_empty(), "{name}");
        }
    }

    const RUST_MACROS: &str = "\
macro_rules! make {
    ($n:ident) => {
        fn $n() {}
    };
}

make!(generated);

pub fn real() {}
";

    #[test]
    fn rust_macro_definitions_are_symbols_and_invocations_are_not() {
        let source = RUST_MACROS;
        assert_eq!(names("src/lib.rs", source), ["make", "real"]);
        let make = find("src/lib.rs", source, "make");
        assert!(source[make.range].starts_with("macro_rules! make"));
    }

    #[test]
    fn same_named_definitions_are_all_returned() {
        let source = "#[cfg(unix)]\nfn f() {}\n#[cfg(not(unix))]\nfn f() {}\n";
        assert_eq!(names("src/lib.rs", source), ["f", "f"]);
    }

    const TS_CLASS: &str = "\
export class Session {
  private token = 1;

  constructor(private readonly id: string) {}

  refresh(force: boolean): boolean {
    return true;
  }

  static create(): Session {
    return new Session('x');
  }

  handler = (e: Event): void => {
    console.log(e);
  };
}

export abstract class Base {
  abstract run(): void;
}
";

    #[test]
    fn ts_classes_name_their_methods_with_a_dot() {
        assert_eq!(
            names("src/session.ts", TS_CLASS),
            [
                "Session.token",
                "Session.constructor",
                "Session.refresh",
                "Session.create",
                "Session.handler",
                "Base.run",
            ]
        );
        let refresh = find("src/session.ts", TS_CLASS, "Session.refresh");
        assert_eq!(
            &TS_CLASS[refresh.signature.clone()],
            "refresh(force: boolean): boolean "
        );
        let handler = find("src/session.ts", TS_CLASS, "Session.handler");
        assert_eq!(
            &TS_CLASS[handler.signature],
            "handler = (e: Event): void => "
        );
    }

    const TS_FUNCTIONS: &str = "\
export function login(user: string): void {
  console.log(user);
}

export const logout = (user: string): void => {
  console.log(user);
};

const short = (n: number) => n + 1;

const LIMIT = 10;

export interface Options {
  retries: number;
}

export type Id = string;

export enum Color {
  Red,
}

function overloaded(a: string): void;
function overloaded(a: number): void;
function overloaded(a: any): void {}
";

    #[test]
    fn ts_functions_arrow_consts_and_types_are_symbols() {
        assert_eq!(
            names("lib/auth.ts", TS_FUNCTIONS),
            [
                "login",
                "logout",
                "short",
                "LIMIT",
                "Options",
                "Id",
                "Color",
                "overloaded",
                "overloaded",
                "overloaded",
            ]
        );
        let login = find("lib/auth.ts", TS_FUNCTIONS, "login");
        assert_eq!(
            &TS_FUNCTIONS[login.signature],
            "export function login(user: string): void "
        );
        let short = find("lib/auth.ts", TS_FUNCTIONS, "short");
        assert_eq!(
            &TS_FUNCTIONS[short.signature.clone()],
            "const short = (n: number) => "
        );
        assert_eq!(&TS_FUNCTIONS[short.body()], "n + 1;");
        let limit = find("lib/auth.ts", TS_FUNCTIONS, "LIMIT");
        assert_eq!(limit.signature, limit.range);
    }

    const TS_NAMESPACE: &str = "\
export namespace Auth {
  export function login(): void {}

  export class Store {
    get(key: string): string {
      return key;
    }
  }
}

declare function ambient(): void;
";

    #[test]
    fn ts_namespaces_prefix_their_members() {
        assert_eq!(
            names("src/auth.ts", TS_NAMESPACE),
            ["Auth.login", "Auth.Store.get", "ambient"]
        );
    }

    const TSX_COMPONENT: &str = "\
import React from 'react';

export function Button({ label }: { label: string }) {
  return <button className=\"btn\">{label}</button>;
}

export const Card = ({ title }: { title: string }) => <div>{title}</div>;

class Panel extends React.Component {
  render() {
    return <section />;
  }
}
";

    #[test]
    fn tsx_parses_jsx_and_the_same_forms() {
        assert_eq!(
            names("web/ui.tsx", TSX_COMPONENT),
            ["Button", "Card", "Panel.render"]
        );
        assert!(
            extract("web/ui.ts", TSX_COMPONENT).is_none(),
            "JSX is not valid in .ts"
        );
    }

    #[test]
    fn comments_inside_an_impl_label_do_not_reach_the_name() {
        let source = "impl fmt::/* note */Display for /* x */ Session {\n    fn fmt(&self) {}\n}\n";
        assert_eq!(
            names("src/s.rs", source),
            ["s::<Session as fmt:: Display>::fmt"]
        );
    }

    /// Every character the coordinator's rule refuses.
    fn hazards() -> Vec<char> {
        let mut all: Vec<char> = (0u32..=0x1f)
            .chain(0x7f..=0x9f)
            .filter_map(char::from_u32)
            .collect();
        for c in ['\u{61c}', '\u{200e}', '\u{200f}', '\u{2028}', '\u{2029}'] {
            all.push(c);
        }
        all.extend(('\u{202a}'..='\u{202e}').chain('\u{2066}'..='\u{2069}'));
        all
    }

    #[test]
    fn every_display_hazard_makes_a_name_non_canonical_and_a_file_unextractable() {
        assert_eq!(hazards().len(), 32 + 33 + 5 + 5 + 4);
        for c in hazards() {
            assert!(
                !is_canonical_name(&format!("a{c}b")),
                "U+{:04X}",
                u32::from(c)
            );
            let source = format!("class A {{\n  \"a{c}b\"() {{}}\n}}\n");
            assert!(
                extract("src/a.ts", &source).is_none(),
                "U+{:04X}",
                u32::from(c)
            );
        }
        assert!(is_canonical_name("a b"));
        assert!(!is_canonical_name(""));
        assert!(!is_canonical_name(" a"));
    }

    #[test]
    fn a_name_the_coordinator_would_refuse_gives_no_symbols() {
        let esc = "class A {\n  \"a\u{1b}b\"() {}\n}\n";
        let bidi = "class A {\n  \"a\u{202e}b\"() {}\n}\n";
        let newline = "class A {\n  [`a\nb`]() {}\n}\n";
        for source in [esc, bidi, newline] {
            assert!(extract("src/a.ts", source).is_none(), "{source:?}");
        }
        assert!(extract("src/a.ts", "class A {\n  \"a b\"() {}\n}\n").is_some());
    }

    #[test]
    fn a_syntax_error_anywhere_gives_no_symbols() {
        assert!(extract("src/a.rs", "fn ok() {}\nfn broken( {\n").is_none());
        assert!(extract("src/a.ts", "function ok() {}\nfunction broken( {\n").is_none());
        assert!(extract("src/a.rs", "fn ok() {}\n").is_some());
    }

    #[test]
    fn a_language_without_a_grammar_gives_no_symbols() {
        assert!(extract("README.md", "# title\n").is_none());
        assert!(extract("web/app.js", "function f() {}\n").is_none());
        assert!(extract("Makefile", "all:\n").is_none());
        assert_eq!(language_of("a/b.mts"), Some(Language::TypeScript));
    }
}
