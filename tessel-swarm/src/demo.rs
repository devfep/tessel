//! The demo repository every run starts from: a small TypeScript shop with twelve numeric
//! functions in seven modules, one test file per function. Node runs it directly
//! (`node --test`), so a run needs no install step. The functions call each other, so a signature
//! change or a rename in one module can break callers in another.

use std::collections::BTreeMap;

/// Repository content: path to file text.
pub type Tree = BTreeMap<String, String>;

/// One function of the starting repository.
pub struct Func {
    pub name: &'static str,
    pub module: &'static str,
    pub params: &'static [&'static str],
    /// Body lines without indentation; the last one is the `return`.
    pub body: &'static [&'static str],
    /// Arguments the function's own test calls it with.
    pub sample: &'static [i64],
    /// The same function in Rust, so tests state the right expected values.
    pub eval: fn(&[i64]) -> i64,
    pub title: &'static str,
}

fn unit_price(a: &[i64]) -> i64 {
    a[0] * a[1]
}
fn apply_discount(a: &[i64]) -> i64 {
    a[0] - a[0] * a[1] / 100
}
fn tax_for(a: &[i64]) -> i64 {
    a[0] * a[1] / 100
}
fn cart_total(a: &[i64]) -> i64 {
    apply_discount(&[unit_price(&a[..2]), a[2]])
}
fn cart_quantity(a: &[i64]) -> i64 {
    (a[0] + a[1] - 1) / a[1]
}
fn available(a: &[i64]) -> i64 {
    (a[0] - a[1]).max(0)
}
fn restock(a: &[i64]) -> i64 {
    a[0] + a[1]
}
fn shipping_cost(a: &[i64]) -> i64 {
    a[0] * 3 + a[1] * 5
}
fn free_shipping_gap(a: &[i64]) -> i64 {
    (a[1] - a[0]).max(0)
}
fn invoice_total(a: &[i64]) -> i64 {
    let net = cart_total(&[a[0], a[1], a[2]]);
    net + tax_for(&[net, a[3]])
}
fn checkout_total(a: &[i64]) -> i64 {
    invoice_total(&[a[0], a[1], 10, 8]) + shipping_cost(&[a[2], 2])
}
fn loyalty_points(a: &[i64]) -> i64 {
    a[0] / a[1]
}

pub const CATALOG: [Func; 12] = [
    Func {
        name: "unitPrice",
        module: "pricing",
        params: &["base", "qty"],
        body: &["return base * qty;"],
        sample: &[250, 3],
        eval: unit_price,
        title: "unitPrice multiplies the base price by the quantity",
    },
    Func {
        name: "applyDiscount",
        module: "pricing",
        params: &["total", "pct"],
        body: &["return total - Math.floor((total * pct) / 100);"],
        sample: &[750, 10],
        eval: apply_discount,
        title: "applyDiscount takes a percentage off",
    },
    Func {
        name: "taxFor",
        module: "pricing",
        params: &["amount", "rate"],
        body: &["return Math.floor((amount * rate) / 100);"],
        sample: &[675, 8],
        eval: tax_for,
        title: "taxFor rounds the tax down",
    },
    Func {
        name: "cartTotal",
        module: "cart",
        params: &["base", "qty", "pct"],
        body: &["return applyDiscount(unitPrice(base, qty), pct);"],
        sample: &[250, 3, 10],
        eval: cart_total,
        title: "cartTotal discounts the line total",
    },
    Func {
        name: "cartQuantity",
        module: "cart",
        params: &["items", "bundle"],
        body: &["return Math.ceil(items / bundle);"],
        sample: &[10, 4],
        eval: cart_quantity,
        title: "cartQuantity rounds bundles up",
    },
    Func {
        name: "available",
        module: "inventory",
        params: &["stock", "reserved"],
        body: &["return Math.max(stock - reserved, 0);"],
        sample: &[10, 3],
        eval: available,
        title: "available never goes below zero",
    },
    Func {
        name: "restock",
        module: "inventory",
        params: &["stock", "batch"],
        body: &["return stock + batch;"],
        sample: &[5, 20],
        eval: restock,
        title: "restock adds a batch",
    },
    Func {
        name: "shippingCost",
        module: "shipping",
        params: &["weight", "zone"],
        body: &["return weight * 3 + zone * 5;"],
        sample: &[4, 2],
        eval: shipping_cost,
        title: "shippingCost charges by weight and zone",
    },
    Func {
        name: "freeShippingGap",
        module: "shipping",
        params: &["total", "threshold"],
        body: &["return Math.max(threshold - total, 0);"],
        sample: &[30, 50],
        eval: free_shipping_gap,
        title: "freeShippingGap shows what is still missing",
    },
    Func {
        name: "invoiceTotal",
        module: "invoice",
        params: &["base", "qty", "pct", "rate"],
        body: &[
            "const net = cartTotal(base, qty, pct);",
            "return net + taxFor(net, rate);",
        ],
        sample: &[250, 3, 10, 8],
        eval: invoice_total,
        title: "invoiceTotal adds tax to the discounted total",
    },
    Func {
        name: "checkoutTotal",
        module: "checkout",
        params: &["base", "qty", "weight"],
        body: &["return invoiceTotal(base, qty, 10, 8) + shippingCost(weight, 2);"],
        sample: &[250, 3, 4],
        eval: checkout_total,
        title: "checkoutTotal adds shipping to the invoice",
    },
    Func {
        name: "loyaltyPoints",
        module: "loyalty",
        params: &["total", "per"],
        body: &["return Math.floor(total / per);"],
        sample: &[729, 10],
        eval: loyalty_points,
        title: "loyaltyPoints awards a point per block",
    },
];

/// The imports each module starts with.
fn imports_of(module: &str) -> &'static str {
    match module {
        "cart" => "import { applyDiscount, unitPrice } from \"./pricing.ts\";\n\n",
        "invoice" => {
            "import { cartTotal } from \"./cart.ts\";\nimport { taxFor } from \"./pricing.ts\";\n\n"
        }
        "checkout" => {
            concat!(
                "import { invoiceTotal } from \"./invoice.ts\";\n",
                "import { shippingCost } from \"./shipping.ts\";\n\n",
            )
        }
        _ => "",
    }
}

pub fn func(name: &str) -> Option<&'static Func> {
    CATALOG.iter().find(|f| f.name == name)
}

impl Func {
    pub fn path(&self) -> String {
        format!("src/{}.ts", self.module)
    }

    pub fn source(&self) -> String {
        let params: Vec<String> = self.params.iter().map(|p| format!("{p}: number")).collect();
        let mut out = format!(
            "export function {}({}): number {{\n",
            self.name,
            params.join(", ")
        );
        for line in self.body {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("}\n");
        out
    }

    pub fn expected(&self) -> i64 {
        (self.eval)(self.sample)
    }

    pub fn sample_args(&self) -> String {
        let args: Vec<String> = self.sample.iter().map(i64::to_string).collect();
        args.join(", ")
    }

    fn test_source(&self) -> String {
        format!(
            "import {{ test }} from \"node:test\";\nimport assert from \"node:assert/strict\";\n\
             import {{ {name} }} from \"../src/{module}.ts\";\n\n\
             test(\"{title}\", () => {{\n  assert.equal({name}({args}), {expected});\n}});\n",
            name = self.name,
            module = self.module,
            title = self.title,
            args = self.sample_args(),
            expected = self.expected(),
        )
    }
}

/// The repository every run starts from.
pub fn base_tree() -> Tree {
    let mut tree = Tree::new();
    tree.insert(
        "package.json".into(),
        "{\n  \"name\": \"swarm-demo\",\n  \"private\": true,\n  \"type\": \"module\",\n  \
         \"scripts\": {\n    \"test\": \"node --test\"\n  }\n}\n"
            .into(),
    );
    tree.insert(".gitignore".into(), "node_modules/\n".into());
    tree.insert("LICENSE".into(), include_str!("../../LICENSE").into());
    for f in &CATALOG {
        let file = tree
            .entry(f.path())
            .or_insert_with(|| imports_of(f.module).to_string());
        if file.ends_with("}\n") {
            file.push('\n');
        }
        file.push_str(&f.source());
        tree.insert(format!("test/{}.test.ts", f.name), f.test_source());
    }
    tree
}

/// Modules whose text mentions `name` in an import or defines it: where new code can call it
/// without adding an import line.
pub fn modules_seeing(tree: &Tree, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (path, text) in tree {
        if !path.starts_with("src/") {
            continue;
        }
        let defines = crate::code::find_function(text, name).is_some();
        let imports = text
            .lines()
            .filter(|l| l.starts_with("import "))
            .any(|l| !crate::code::word_positions(l, name).is_empty());
        if defines || imports {
            out.push(path.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_tree_has_a_source_and_a_test_per_function() {
        let tree = base_tree();
        for f in &CATALOG {
            assert!(tree[&f.path()].contains(&f.source()), "{}", f.name);
            assert!(tree.contains_key(&format!("test/{}.test.ts", f.name)));
        }
        assert_eq!(tree.keys().filter(|p| p.starts_with("src/")).count(), 7);
    }

    #[test]
    fn the_demo_repository_is_mit_licensed() {
        let tree = base_tree();
        let license = &tree["LICENSE"];
        assert!(license.starts_with("MIT License\n"), "{license}");
        assert!(license.contains("Copyright (c) 2026 "), "{license}");
        assert!(license.contains("Permission is hereby granted, free of charge"));
    }

    #[test]
    fn expected_values_match_hand_calculation() {
        let want = [
            ("unitPrice", 750),
            ("applyDiscount", 675),
            ("taxFor", 54),
            ("cartTotal", 675),
            ("cartQuantity", 3),
            ("available", 7),
            ("restock", 25),
            ("shippingCost", 22),
            ("freeShippingGap", 20),
            ("invoiceTotal", 729),
            ("checkoutTotal", 751),
            ("loyaltyPoints", 72),
        ];
        for (name, value) in want {
            assert_eq!(func(name).map(Func::expected), Some(value), "{name}");
        }
    }

    #[test]
    fn modules_seeing_a_function_include_its_importers() {
        let tree = base_tree();
        assert_eq!(
            modules_seeing(&tree, "unitPrice"),
            ["src/cart.ts", "src/pricing.ts"]
        );
        assert_eq!(modules_seeing(&tree, "loyaltyPoints"), ["src/loyalty.ts"]);
    }

    #[test]
    fn every_function_parses_back_out_of_its_module() {
        let tree = base_tree();
        for f in &CATALOG {
            assert!(
                crate::code::find_function(&tree[&f.path()], f.name).is_some(),
                "{}",
                f.name
            );
        }
    }
}
