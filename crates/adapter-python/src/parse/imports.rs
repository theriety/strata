//! Import-statement and `__all__` extraction for module-level statements.

use rustpython_parser::ast::{Alias, Constant, Expr};
use smol_str::SmolStr;

use super::Import;

/// Builds an [`Import`] from a plain `import a, b.c as d` statement.
pub(super) fn plain_import(aliases: &[Alias]) -> Import {
    let mut names = Vec::new();
    let mut targets = Vec::new();
    for alias in aliases {
        let dotted = alias.name.as_str();
        // `import a.b.c` binds `a`; `import a.b.c as d` binds `d`.
        let bound = alias.asname.as_ref().map_or_else(
            || SmolStr::new(dotted.split('.').next().unwrap_or(dotted)),
            |asname| SmolStr::new(asname.as_str()),
        );
        names.push(bound);
        targets.push(SmolStr::new(dotted));
    }
    Import {
        module: SmolStr::new(""),
        level: 0,
        names,
        targets,
        star: false,
    }
}

/// Builds an [`Import`] from a `from module import x, y as z` statement.
pub(super) fn from_import(from: &rustpython_parser::ast::StmtImportFrom) -> Import {
    let module = from
        .module
        .as_ref()
        .map_or_else(|| SmolStr::new(""), |module| SmolStr::new(module.as_str()));
    let level = from
        .level
        .as_ref()
        .map_or(0, rustpython_parser::ast::Int::to_u32);
    let star = from.names.iter().any(|alias| alias.name.as_str() == "*");
    let mut names = Vec::new();
    let mut targets = Vec::new();
    if !star {
        for alias in &from.names {
            let original = SmolStr::new(alias.name.as_str());
            let bound = alias
                .asname
                .as_ref()
                .map_or_else(|| original.clone(), |asname| SmolStr::new(asname.as_str()));
            names.push(bound);
            targets.push(original);
        }
    }
    Import {
        module,
        level,
        names,
        targets,
        star,
    }
}

/// Records the string entries of a module-level `__all__ = [...]` assignment.
pub(super) fn collect_dunder_all(targets: &[Expr], value: &Expr, out: &mut Vec<SmolStr>) {
    let assigns_dunder_all = targets
        .iter()
        .any(|target| matches!(target, Expr::Name(name) if name.id.as_str() == "__all__"));
    if !assigns_dunder_all {
        return;
    }
    let elements = match value {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return,
    };
    for element in elements {
        if let Expr::Constant(constant) = element
            && let Constant::Str(text) = &constant.value
        {
            out.push(SmolStr::new(text.as_str()));
        }
    }
}
