//! Text surgery on the demo's TypeScript. The demo is generated, so only the shapes the
//! generator writes are handled: top-level `export function name(params): number {` blocks that
//! end with a closing brace in column zero.

use std::ops::Range;

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// Offsets of the whole-word occurrences of `name`.
pub fn word_positions(src: &str, name: &str) -> Vec<usize> {
    let bytes = src.as_bytes();
    src.match_indices(name)
        .map(|(at, _)| at)
        .filter(|&at| at == 0 || !is_ident(bytes[at - 1]))
        .filter(|&at| bytes.get(at + name.len()).is_none_or(|&b| !is_ident(b)))
        .collect()
}

/// The offset of the `)` that closes the `(` at `open`.
fn matching_paren(src: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, byte) in src.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// Where one exported function sits in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Func {
    pub name: String,
    /// The whole definition, from `export` to the newline after the closing brace.
    pub range: Range<usize>,
    /// The `)` that ends the parameter list.
    pub params_close: usize,
    /// The first line: everything the signature consists of.
    pub header: Range<usize>,
}

pub fn functions(src: &str) -> Vec<Func> {
    const HEAD: &str = "export function ";
    let mut out = Vec::new();
    for (start, _) in src.match_indices(HEAD) {
        if start != 0 && src.as_bytes()[start - 1] != b'\n' {
            continue;
        }
        let rest = &src[start + HEAD.len()..];
        let Some(paren) = rest.find('(') else {
            continue;
        };
        let open = start + HEAD.len() + paren;
        let Some(params_close) = matching_paren(src, open) else {
            continue;
        };
        let Some(close) = src[start..].find("\n}\n") else {
            continue;
        };
        let header_end = src[start..].find('\n').map_or(src.len(), |at| start + at);
        out.push(Func {
            name: rest[..paren].to_string(),
            range: start..start + close + 3,
            params_close,
            header: start..header_end,
        });
    }
    out
}

pub fn find_function(src: &str, name: &str) -> Option<Func> {
    functions(src).into_iter().find(|f| f.name == name)
}

/// How many parameters `func` declares.
pub fn param_count(src: &str, func: &Func) -> usize {
    let open = func.range.start + "export function ".len() + func.name.len();
    let params = &src[open + 1..func.params_close];
    if params.trim().is_empty() {
        0
    } else {
        params.matches(',').count() + 1
    }
}

/// Rewrites the last `return EXPR;` line of `func`'s body with `rewrite(EXPR)`.
fn map_last_return(src: &str, func: &Func, rewrite: impl Fn(&str) -> String) -> Option<String> {
    const MARK: &str = "\n  return ";
    let body = &src[func.params_close..func.range.end];
    let at = func.params_close + body.rfind(MARK)?;
    let expr_start = at + MARK.len();
    let expr_end = expr_start + src[expr_start..].find(";\n")?;
    let mut out = String::with_capacity(src.len() + 32);
    out.push_str(&src[..at]);
    out.push_str(&rewrite(&src[expr_start..expr_end]));
    out.push_str(&src[expr_end + 1..]);
    Some(out)
}

/// Adds the required parameter `param` to `name` and scales its result by it.
pub fn add_scale_param(src: &str, name: &str, param: &str) -> Option<String> {
    let func = find_function(src, name)?;
    let scaled = map_last_return(src, &func, |expr| format!("\n  return ({expr}) * {param};"))?;
    let with_param = functions(&scaled)
        .into_iter()
        .find(|f| f.name == name)?
        .params_close;
    let mut out = scaled;
    out.insert_str(with_param, &format!(", {param}: number"));
    Some(out)
}

/// Passes `arg` as one more argument in every call of `name`. The definition is left alone.
pub fn add_call_arg(src: &str, name: &str, arg: &str) -> String {
    let mut out = src.to_string();
    for at in word_positions(src, name).into_iter().rev() {
        let open = at + name.len();
        if !out[open..].starts_with('(') || out[..at].ends_with("function ") {
            continue;
        }
        if let Some(close) = matching_paren(&out, open) {
            out.insert_str(close, &format!(", {arg}"));
        }
    }
    out
}

/// Replaces every whole-word `from` with `to`.
pub fn rename_word(src: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut last = 0;
    for at in word_positions(src, from) {
        out.push_str(&src[last..at]);
        out.push_str(to);
        last = at + from.len();
    }
    out.push_str(&src[last..]);
    out
}

/// Names the last return value instead of returning the expression: same behaviour, new text.
pub fn name_the_result(src: &str, name: &str, var: &str) -> Option<String> {
    let func = find_function(src, name)?;
    map_last_return(src, &func, |expr| {
        format!("\n  const {var} = {expr};\n  return {var};")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = concat!(
        "import { a } from \"./a.ts\";\n\n",
        "export function unitPrice(base: number, qty: number): number {\n",
        "  return base * qty;\n}\n\n",
        "export function cartTotal(base: number, qty: number): number {\n",
        "  const x = 1;\n  return unitPrice(base, qty) + x;\n}\n",
    );

    #[test]
    fn finds_functions_and_counts_parameters() {
        let found = functions(SRC);
        assert_eq!(
            found.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["unitPrice", "cartTotal"]
        );
        assert_eq!(param_count(SRC, &found[0]), 2);
        assert_eq!(
            &SRC[found[0].header.clone()],
            "export function unitPrice(base: number, qty: number): number {"
        );
    }

    #[test]
    fn scale_param_changes_signature_and_result() {
        let out = add_scale_param(SRC, "unitPrice", "k3").unwrap_or_default();
        assert!(out.contains("unitPrice(base: number, qty: number, k3: number): number {"));
        assert!(out.contains("  return (base * qty) * k3;"));
        assert!(out.contains("export function cartTotal"));
    }

    #[test]
    fn call_args_skip_the_definition_and_handle_nesting() {
        let out = add_call_arg("f(f(1, 2), 3)\nexport function f(a: number) {\n", "f", "9");
        assert_eq!(out, "f(f(1, 2, 9), 3, 9)\nexport function f(a: number) {\n");
        assert_eq!(add_call_arg("fx(1) xf(2)", "f", "9"), "fx(1) xf(2)");
    }

    #[test]
    fn rename_replaces_whole_words_only() {
        assert_eq!(
            rename_word("unitPrice(1) unitPrices xunitPrice", "unitPrice", "up"),
            "up(1) unitPrices xunitPrice"
        );
    }

    #[test]
    fn naming_the_result_keeps_the_last_return() {
        let out = name_the_result(SRC, "cartTotal", "out4").unwrap_or_default();
        assert!(out.contains("  const out4 = unitPrice(base, qty) + x;\n  return out4;\n}"));
    }
}
