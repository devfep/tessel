//! Scripted tasks: what they are, how a seed produces them, how each edit is applied to a
//! checkout, and which scopes an edit touches and therefore needs to claim.
//!
//! Every task is written against the starting repository and also reads the checkout it is
//! applied to, as a real agent would: it finds the function by lineage (a renamed function is
//! still found), passes as many arguments as the function now takes, and updates every caller
//! that exists in that checkout. Run on the starting repository it is a valid change on its own
//! (the repository's tests still pass). Two tasks together may not be.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use tessel_coordinator::protocol::{Mode, Scope, ScopeClaim, SymbolId};

use crate::code::{self, add_call_arg, add_scale_param, name_the_result, rename_word};
use crate::demo::{self, Tree, CATALOG};
use crate::rng::Rng;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// Rewrites the body of `func`; behaviour is unchanged.
    Body,
    /// Adds a required parameter that scales the result and passes `1` from every caller.
    Signature,
    /// Adds `extraN` to `host`, a new function that calls `func`, with a test for it.
    Add { host: String, constant: i64 },
    /// Renames `func` everywhere.
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    /// 1-based; also the order the uncoordinated replay merges in.
    pub id: usize,
    /// The catalog name the task targets, whatever it is called by now.
    pub func: String,
    #[serde(flatten)]
    pub kind: Kind,
}

impl Task {
    pub fn label(&self) -> String {
        format!("t{:02}", self.id)
    }

    /// One line for the intent shown to other agents.
    pub fn intent(&self) -> String {
        let what = match &self.kind {
            Kind::Body => format!("rewrite the body of {}", self.func),
            Kind::Signature => format!("add a parameter to {}", self.func),
            Kind::Add { host, .. } => {
                format!("add extra{} to {host} calling {}", self.id, self.func)
            }
            Kind::Rename => format!("rename {}", self.func),
        };
        format!("{}: {what}", self.label())
    }
}

/// `count` tasks for `seed`. With probability `overlap` a task targets a function an earlier task
/// already targeted; otherwise it targets one no earlier task did (while any is left).
pub fn generate(seed: u64, count: usize, overlap: f64) -> Result<Vec<Task>> {
    if !(0.0..=1.0).contains(&overlap) {
        bail!("overlap must be between 0 and 1, got {overlap}");
    }
    let tree = demo::base_tree();
    let mut rng = Rng::new(seed);
    let mut targeted: Vec<&'static str> = Vec::new();
    let mut tasks = Vec::with_capacity(count);
    for id in 1..=count {
        let roll = rng.below(100);
        let reuse = !targeted.is_empty() && rng.chance(overlap);
        let func = if reuse {
            targeted[rng.below(targeted.len())]
        } else {
            let fresh: Vec<&'static str> = CATALOG
                .iter()
                .map(|f| f.name)
                .filter(|name| !targeted.contains(name))
                .collect();
            if fresh.is_empty() {
                CATALOG[rng.below(CATALOG.len())].name
            } else {
                fresh[rng.below(fresh.len())]
            }
        };
        if !targeted.contains(&func) {
            targeted.push(func);
        }
        let kind = match roll {
            0..=29 => Kind::Body,
            30..=54 => Kind::Signature,
            55..=84 => {
                let hosts = demo::modules_seeing(&tree, func);
                let host = hosts[rng.below(hosts.len())].clone();
                Kind::Add {
                    host,
                    constant: 1 + i64::try_from(rng.below(9)).unwrap_or(0),
                }
            }
            _ => Kind::Rename,
        };
        tasks.push(Task {
            id,
            func: func.to_string(),
            kind,
        });
    }
    Ok(tasks)
}

/// How many tasks target a function an earlier task also targeted.
pub fn repeated_targets(tasks: &[Task]) -> usize {
    let distinct: BTreeSet<&str> = tasks.iter().map(|t| t.func.as_str()).collect();
    tasks.len() - distinct.len()
}

/// The module and current name of the function that started life as `origin`. A rename appends
/// `R<id>`, so the lineage is the original name followed by any number of those.
pub fn resolve(tree: &Tree, origin: &str) -> Option<(String, String)> {
    for (path, text) in tree.iter().filter(|(p, _)| p.starts_with("src/")) {
        for func in code::functions(text) {
            let Some(rest) = func.name.strip_prefix(origin) else {
                continue;
            };
            let renames = rest.split('R').skip(1);
            let lineage = rest.is_empty() || rest.starts_with('R');
            if lineage
                && renames
                    .clone()
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            {
                return Some((path.clone(), func.name));
            }
        }
    }
    None
}

/// The checkout after the task's edit.
pub fn apply(task: &Task, tree: &Tree) -> Result<Tree> {
    let (path, current) = resolve(tree, &task.func).ok_or_else(|| {
        anyhow!(
            "{}: no function {} in the checkout",
            task.label(),
            task.func
        )
    })?;
    let mut next = tree.clone();
    match &task.kind {
        Kind::Body => {
            let var = format!("out{}", task.id);
            let edited = name_the_result(&tree[&path], &current, &var)
                .ok_or_else(|| anyhow!("{}: {current} has no return to rewrite", task.label()))?;
            next.insert(path, edited);
        }
        Kind::Signature => {
            let param = format!("k{}", task.id);
            let edited = add_scale_param(&tree[&path], &current, &param)
                .ok_or_else(|| anyhow!("{}: {current} has no return to scale", task.label()))?;
            next.insert(path, edited);
            for text in next.values_mut() {
                *text = add_call_arg(text, &current, "1");
            }
        }
        Kind::Rename => {
            let renamed = format!("{current}R{}", task.id);
            for text in next.values_mut() {
                *text = rename_word(text, &current, &renamed);
            }
        }
        Kind::Add { host, constant } => {
            add_function(task, tree, &mut next, (&path, &current), (host, *constant))?;
        }
    }
    Ok(next)
}

fn add_function(
    task: &Task,
    tree: &Tree,
    next: &mut Tree,
    callee: (&str, &str),
    added: (&str, i64),
) -> Result<()> {
    let (callee_path, callee_name) = callee;
    let (host, constant) = added;
    let base =
        demo::func(&task.func).ok_or_else(|| anyhow!("{} is not in the catalog", task.func))?;
    let host_src = tree
        .get(host)
        .ok_or_else(|| anyhow!("{}: host module {host} is missing", task.label()))?;
    let def = code::find_function(&tree[callee_path], callee_name)
        .ok_or_else(|| anyhow!("{}: {callee_name} vanished", task.label()))?;
    let arity = code::param_count(&tree[callee_path], &def);
    let name = format!("extra{}", task.id);
    let params: Vec<String> = (0..base.params.len())
        .map(|i| format!("p{i}: number"))
        .collect();
    let args: Vec<String> = (0..arity)
        .map(|i| {
            if i < base.params.len() {
                format!("p{i}")
            } else {
                "1".into()
            }
        })
        .collect();
    let appended = format!(
        "\nexport function {name}({}): number {{\n  return {callee_name}({}) + {constant};\n}}\n",
        params.join(", "),
        args.join(", ")
    );
    next.insert(host.to_string(), format!("{host_src}{appended}"));
    let module_of = |path: &str| path.trim_start_matches("src/").to_string();
    next.insert(
        format!("test/{name}.test.ts"),
        format!(
            "import {{ test }} from \"node:test\";\nimport assert from \"node:assert/strict\";\n\
             import {{ {name} }} from \"../src/{host_module}\";\n\
             import {{ {callee_name} }} from \"../src/{callee_module}\";\n\n\
             test(\"{name} adds {constant} to {origin}\", () => {{\n  \
             assert.equal(typeof {callee_name}, \"function\");\n  \
             assert.equal({name}({sample}), {expected});\n}});\n",
            host_module = module_of(host),
            callee_module = module_of(callee_path),
            origin = task.func,
            sample = base.sample_args(),
            expected = base.expected() + constant,
        ),
    );
    Ok(())
}

// ---------- scopes ----------

type Shape = (BTreeMap<String, (String, String)>, String);

fn shape(src: &str) -> Shape {
    let mut symbols = BTreeMap::new();
    let mut residual = String::new();
    let mut at = 0;
    for func in code::functions(src) {
        residual.push_str(&src[at..func.range.start]);
        residual.push('\0');
        at = func.range.end;
        symbols.insert(
            func.name.clone(),
            (
                src[func.header.clone()].to_string(),
                src[func.header.end..func.range.end].to_string(),
            ),
        );
    }
    residual.push_str(&src[at..]);
    (symbols, residual)
}

fn symbol(path: &str, name: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::Symbol(SymbolId {
            path: path.to_string(),
            qualified_name: name.to_string(),
        }),
        mode,
    }
}

fn file(path: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File {
            path: path.to_string(),
        },
        mode,
    }
}

/// What the edit from `before` to `after` touched, as the CLI's `changed_scopes` reports it: a
/// symbol whose signature changed or that vanished is `edit_signature`, one whose body alone
/// changed is `edit_body`, a new one is `create`, a change outside every function is `edit_body`
/// on the file, and a new file is `create` on the file.
pub fn touched(before: &Tree, after: &Tree) -> Vec<ScopeClaim> {
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut out = Vec::new();
    for path in paths {
        match (before.get(path), after.get(path)) {
            (None, Some(_)) => out.push(file(path, Mode::Create)),
            (Some(_), None) => out.push(file(path, Mode::EditSignature)),
            (Some(old), Some(new)) if old != new => out.extend(changed_in(path, old, new)),
            (Some(_), Some(_)) | (None, None) => {}
        }
    }
    out
}

fn changed_in(path: &str, old: &str, new: &str) -> Vec<ScopeClaim> {
    let ((was, was_rest), (now, now_rest)) = (shape(old), shape(new));
    let mut out = Vec::new();
    for (name, (header, body)) in &was {
        match now.get(name) {
            None => out.push(symbol(path, name, Mode::EditSignature)),
            Some((h, _)) if h != header => out.push(symbol(path, name, Mode::EditSignature)),
            Some((_, b)) if b != body => out.push(symbol(path, name, Mode::EditBody)),
            Some(_) => {}
        }
    }
    for name in now.keys().filter(|n| !was.contains_key(*n)) {
        out.push(symbol(path, name, Mode::Create));
    }
    if was_rest != now_rest {
        out.push(file(path, Mode::EditBody));
    }
    out
}

/// What to claim for `touched`, the way the CLI plans it: a single changed symbol is claimed as
/// that symbol; anything more in a file claims the file, in the modes that cover the work.
pub fn plan_claims(touched: &[ScopeClaim]) -> Vec<ScopeClaim> {
    let mut by_path: BTreeMap<String, Vec<&ScopeClaim>> = BTreeMap::new();
    for claim in touched {
        let path = match &claim.scope {
            Scope::Dir { path } | Scope::File { path } => path.clone(),
            Scope::Symbol(s) => s.path.clone(),
        };
        by_path.entry(path).or_default().push(claim);
    }
    let mut out = Vec::new();
    for (path, group) in by_path {
        if let [only] = group.as_slice() {
            if matches!(only.scope, Scope::Symbol(_)) && only.mode != Mode::Create {
                out.push((*only).clone());
                continue;
            }
        }
        let has = |wanted: Mode| group.iter().any(|c| c.mode == wanted);
        if has(Mode::EditSignature) {
            out.push(file(&path, Mode::EditSignature));
        } else if has(Mode::EditBody) {
            out.push(file(&path, Mode::EditBody));
        }
        if has(Mode::Create) {
            out.push(file(&path, Mode::Create));
        }
    }
    out
}

/// What the task depends on without changing: an added function relies on its callee.
pub fn dependencies(task: &Task, tree: &Tree) -> Vec<ScopeClaim> {
    match (&task.kind, resolve(tree, &task.func)) {
        (Kind::Add { .. }, Some((path, name))) => vec![symbol(&path, &name, Mode::Depend)],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::base_tree;

    fn task(id: usize, func: &str, kind: Kind) -> Task {
        Task {
            id,
            func: func.into(),
            kind,
        }
    }

    #[test]
    fn same_seed_same_tasks_and_a_different_seed_differs() {
        let first = generate(5, 12, 0.5).unwrap();
        assert_eq!(first, generate(5, 12, 0.5).unwrap());
        assert_ne!(first, generate(6, 12, 0.5).unwrap());
        assert_eq!(
            first.iter().map(|t| t.id).collect::<Vec<_>>(),
            (1..=12).collect::<Vec<_>>()
        );
    }

    #[test]
    fn overlap_rate_decides_how_many_tasks_share_a_target() {
        for seed in 0..20 {
            let none = generate(seed, 10, 0.0).unwrap();
            assert_eq!(repeated_targets(&none), 0, "seed {seed}");
            let all = generate(seed, 10, 1.0).unwrap();
            assert_eq!(repeated_targets(&all), 9, "seed {seed}");
        }
        let sum = |rate: f64| -> usize {
            (0..40)
                .map(|s| repeated_targets(&generate(s, 10, rate).unwrap()))
                .sum()
        };
        assert!(
            sum(0.2) < sum(0.5) && sum(0.5) < sum(0.8),
            "{} {} {}",
            sum(0.2),
            sum(0.5),
            sum(0.8)
        );
    }

    #[test]
    fn invalid_overlap_is_refused() {
        assert!(generate(1, 3, 1.5).is_err());
        assert!(generate(1, 3, -0.1).is_err());
    }

    #[test]
    fn body_edit_names_the_result_without_touching_other_functions() {
        let base = base_tree();
        let after = apply(&task(1, "cartTotal", Kind::Body), &base).unwrap();
        assert!(
            after["src/cart.ts"].contains("const out1 = applyDiscount(unitPrice(base, qty), pct);")
        );
        assert_eq!(
            touched(&base, &after),
            [symbol("src/cart.ts", "cartTotal", Mode::EditBody)]
        );
    }

    #[test]
    fn signature_edit_updates_the_definition_every_caller_and_the_test() {
        let base = base_tree();
        let after = apply(&task(2, "unitPrice", Kind::Signature), &base).unwrap();
        assert!(after["src/pricing.ts"]
            .contains("unitPrice(base: number, qty: number, k2: number): number {"));
        assert!(after["src/cart.ts"].contains("applyDiscount(unitPrice(base, qty, 1), pct)"));
        assert!(after["test/unitPrice.test.ts"].contains("unitPrice(250, 3, 1)"));
        let got = touched(&base, &after);
        assert!(got.contains(&symbol("src/pricing.ts", "unitPrice", Mode::EditSignature)));
        assert!(got.contains(&symbol("src/cart.ts", "cartTotal", Mode::EditBody)));
        assert!(got.contains(&file("test/unitPrice.test.ts", Mode::EditBody)));
    }

    #[test]
    fn rename_follows_the_lineage_and_touches_the_old_and_new_symbol() {
        let base = base_tree();
        let once = apply(&task(3, "taxFor", Kind::Rename), &base).unwrap();
        assert_eq!(
            resolve(&once, "taxFor"),
            Some(("src/pricing.ts".into(), "taxForR3".into()))
        );
        assert!(once["src/invoice.ts"].contains("import { taxForR3 }"));
        let twice = apply(&task(4, "taxFor", Kind::Rename), &once).unwrap();
        assert_eq!(
            resolve(&twice, "taxFor").map(|r| r.1),
            Some("taxForR3R4".into())
        );
        let got = touched(&base, &once);
        assert!(got.contains(&symbol("src/pricing.ts", "taxFor", Mode::EditSignature)));
        assert!(got.contains(&symbol("src/pricing.ts", "taxForR3", Mode::Create)));
        assert!(resolve(&base, "tax").is_none());
    }

    #[test]
    fn added_function_adapts_to_the_callee_it_finds() {
        let base = base_tree();
        let add = task(
            5,
            "unitPrice",
            Kind::Add {
                host: "src/cart.ts".into(),
                constant: 7,
            },
        );
        let stale = apply(&add, &base).unwrap();
        assert!(stale["src/cart.ts"].contains("return unitPrice(p0, p1) + 7;"));
        assert!(stale["test/extra5.test.ts"].contains("assert.equal(extra5(250, 3), 757);"));
        let wider = apply(&task(6, "unitPrice", Kind::Signature), &base).unwrap();
        let fresh = apply(&add, &wider).unwrap();
        assert!(fresh["src/cart.ts"].contains("return unitPrice(p0, p1, 1) + 7;"));
        let got = touched(&wider, &fresh);
        assert!(got.contains(&symbol("src/cart.ts", "extra5", Mode::Create)));
        assert!(got.contains(&file("test/extra5.test.ts", Mode::Create)));
        assert!(
            got.contains(&file("src/cart.ts", Mode::EditBody)),
            "the gap between symbols is outside every symbol"
        );
        assert_eq!(
            dependencies(&add, &base),
            [symbol("src/pricing.ts", "unitPrice", Mode::Depend)]
        );
    }

    #[test]
    fn claims_follow_the_cli_rules() {
        let base = base_tree();
        let sig = apply(&task(1, "unitPrice", Kind::Signature), &base).unwrap();
        let claims = plan_claims(&touched(&base, &sig));
        assert!(claims.contains(&symbol("src/pricing.ts", "unitPrice", Mode::EditSignature)));
        assert!(claims.contains(&symbol("src/cart.ts", "cartTotal", Mode::EditBody)));
        let add = task(
            2,
            "unitPrice",
            Kind::Add {
                host: "src/cart.ts".into(),
                constant: 1,
            },
        );
        let after = apply(&add, &base).unwrap();
        let claims = plan_claims(&touched(&base, &after));
        assert!(claims.contains(&file("src/cart.ts", Mode::Create)));
        assert!(claims.contains(&file("test/extra2.test.ts", Mode::Create)));
        let ours = touched(&base, &after);
        assert!(tessel_coordinator::protocol::uncovered(&claims, &ours).is_empty());
    }

    #[test]
    fn every_task_alone_leaves_the_functions_it_names_in_place() {
        let base = base_tree();
        for seed in 0..10 {
            for t in generate(seed, 8, 0.5).unwrap() {
                let after = apply(&t, &base).unwrap();
                assert_ne!(after, base, "{}", t.intent());
            }
        }
    }
}
