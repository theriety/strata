//! Import-statement extraction for module-level statements.

use rustpython_parser::ast::{Alias, ExceptHandler, Stmt};
use smol_str::SmolStr;

use super::Import;

/// Builds an [`Import`] from a plain `import a, b.c as d` statement.
pub(super) fn plain_import(aliases: &[Alias]) -> Import {
    let mut names = Vec::new();
    let mut targets = Vec::new();
    let mut module_aliases = Vec::new();
    for alias in aliases {
        let dotted = alias.name.as_str();
        // `import a.b.c` binds `a`; `import a.b.c as d` binds `d`.
        let bound = alias.asname.as_ref().map_or_else(
            || SmolStr::new(dotted.split('.').next().unwrap_or(dotted)),
            |asname| SmolStr::new(asname.as_str()),
        );
        if alias.asname.is_some() {
            module_aliases.push(bound.clone());
        }
        names.push(bound);
        targets.push(SmolStr::new(dotted));
    }
    Import {
        module: SmolStr::new(""),
        level: 0,
        names,
        targets,
        star: false,
        aliases: module_aliases,
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
        aliases: Vec::new(),
    }
}

/// Collects the imports nested in module-level compound statements: the `if`,
/// `try`, `with`, loop and `match` blocks that guard an optional or typing-only
/// import. Imports in a `def` or `class` body are not module bindings and are
/// left out, as are the plain top-level imports (see [`from_import`]).
pub(super) fn conditional_imports(suite: &[Stmt]) -> Vec<Import> {
    let mut found = Vec::new();
    for statement in suite {
        for block in child_blocks(statement) {
            collect_block(block, &mut found);
        }
    }
    found
}

/// Appends the imports of `block`, descending into its compound statements.
fn collect_block(block: &[Stmt], found: &mut Vec<Import>) {
    for statement in block {
        match statement {
            Stmt::Import(import) => found.push(plain_import(&import.names)),
            Stmt::ImportFrom(from) => found.push(from_import(from)),
            other => {
                for inner in child_blocks(other) {
                    collect_block(inner, found);
                }
            }
        }
    }
}

/// The statement blocks a compound statement runs, in source order.
pub(super) fn child_blocks(statement: &Stmt) -> Vec<&[Stmt]> {
    match statement {
        Stmt::If(block) => vec![&block.body, &block.orelse],
        Stmt::For(block) => vec![&block.body, &block.orelse],
        Stmt::AsyncFor(block) => vec![&block.body, &block.orelse],
        Stmt::While(block) => vec![&block.body, &block.orelse],
        Stmt::With(block) => vec![&block.body],
        Stmt::AsyncWith(block) => vec![&block.body],
        Stmt::Try(block) => try_blocks(
            &block.body,
            &block.handlers,
            &block.orelse,
            &block.finalbody,
        ),
        Stmt::TryStar(block) => try_blocks(
            &block.body,
            &block.handlers,
            &block.orelse,
            &block.finalbody,
        ),
        Stmt::Match(block) => block
            .cases
            .iter()
            .map(|case| case.body.as_slice())
            .collect(),
        _ => Vec::new(),
    }
}

/// The blocks of a `try` statement: body, each handler, `else`, `finally`.
fn try_blocks<'src>(
    body: &'src [Stmt],
    handlers: &'src [ExceptHandler],
    orelse: &'src [Stmt],
    finalbody: &'src [Stmt],
) -> Vec<&'src [Stmt]> {
    let mut blocks = vec![body];
    blocks.extend(handlers.iter().map(|handler| {
        let ExceptHandler::ExceptHandler(clause) = handler;
        clause.body.as_slice()
    }));
    blocks.push(orelse);
    blocks.push(finalbody);
    blocks
}
