//! Module objects imported whole: `import a`, `import a.b as c`, `from pkg import sub`.
//!
//! The binder resolves `from m import x` to the node of `x`, but `a.foo()` goes
//! through the module object, so no edge reaches `foo`. A production module that
//! imports a module this way may use any of its members; the target's
//! declarations are exempted from advice rather than reported as test-only.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{Node, NodeId};

use super::polarity::{is_support_path, is_test_path};
use super::resolver::Resolver;
use crate::parse::{Import, ParsedModule};

/// Marks every declaration of a module imported whole by production code as
/// `advice_exempt`.
pub(super) fn exempt_module_objects(
    modules: &[ParsedModule],
    resolver: &Resolver,
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    nodes: &mut [Node],
) {
    for module in modules {
        if is_test_path(&module.path) || is_support_path(&module.path) {
            continue;
        }
        for import in module.imports.iter().filter(|import| !import.star) {
            // `import a.b` names the module in `targets`; `from pkg import sub`
            // may bind a submodule, whose path is `pkg.sub`.
            let plain = import.level == 0 && import.module.is_empty();
            for name in &import.targets {
                let path = if plain {
                    resolver.module_of_dotted(name).cloned()
                } else {
                    let joined = if import.module.is_empty() {
                        name.clone()
                    } else {
                        SmolStr::new(format!("{}.{name}", import.module))
                    };
                    resolver.resolve(
                        &module.path,
                        &Import {
                            module: joined,
                            names: Vec::new(),
                            targets: Vec::new(),
                            ..import.clone()
                        },
                    )
                };
                let Some(path) = path else { continue };
                for id in local.get(&path).into_iter().flat_map(HashMap::values) {
                    if let Some(node) = nodes.get_mut(id.0 as usize) {
                        node.advice_exempt = true;
                    }
                }
            }
        }
    }
}
