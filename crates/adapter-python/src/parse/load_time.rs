//! Import statements that run while their module loads.
//!
//! These feed the file-import facts. A statement is left out when its target is
//! not loaded with the module: an import under `if TYPE_CHECKING:` never runs,
//! and one in a function body runs only when the function is called (a lazy
//! import, which cannot take part in a load-time cycle).

use rustpython_parser::ast::{Expr, Ranged, Stmt, UnaryOp};

use super::imports::{child_blocks, from_import, plain_import};
use super::{Import, ImportSite};

/// How an `if` test relates to `typing.TYPE_CHECKING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guard {
    /// `if TYPE_CHECKING:` - the body runs only for a type checker.
    TypeChecking,
    /// `if not TYPE_CHECKING:` - the `else` runs only for a type checker.
    NotTypeChecking,
    /// Any other test; both branches may run.
    Other,
}

/// Collects the load-time imports of `suite` in source order.
pub(super) fn load_time_imports(suite: &[Stmt], source: &str) -> Vec<ImportSite> {
    let mut sites = Vec::new();
    collect_block(suite, source, &mut sites);
    sites
}

/// Appends the load-time imports of `block`, descending into the compound
/// statements that run at load time. Function bodies are not entered.
fn collect_block(block: &[Stmt], source: &str, sites: &mut Vec<ImportSite>) {
    for statement in block {
        match statement {
            Stmt::Import(import) => {
                sites.push(site(statement, source, plain_import(&import.names)));
            }
            Stmt::ImportFrom(from) => sites.push(site(statement, source, from_import(from))),
            Stmt::If(branch) => match guard(&branch.test) {
                Guard::TypeChecking => collect_block(&branch.orelse, source, sites),
                Guard::NotTypeChecking => collect_block(&branch.body, source, sites),
                Guard::Other => {
                    collect_block(&branch.body, source, sites);
                    collect_block(&branch.orelse, source, sites);
                }
            },
            Stmt::ClassDef(class) => collect_block(&class.body, source, sites),
            other => {
                for inner in child_blocks(other) {
                    collect_block(inner, source, sites);
                }
            }
        }
    }
}

/// Pairs an import with the line its statement starts on.
fn site(statement: &Stmt, source: &str, import: Import) -> ImportSite {
    let offset = usize::from(statement.start());
    let before = source.get(..offset).unwrap_or(source);
    let newlines = before.bytes().filter(|&byte| byte == b'\n').count();
    ImportSite {
        line: u32::try_from(newlines + 1).unwrap_or(u32::MAX),
        import,
    }
}

/// Classifies an `if` test as a `TYPE_CHECKING` guard, a negated one, or neither.
fn guard(test: &Expr) -> Guard {
    match test {
        Expr::UnaryOp(unary) if unary.op == UnaryOp::Not => {
            if is_type_checking(&unary.operand) {
                Guard::NotTypeChecking
            } else {
                Guard::Other
            }
        }
        other if is_type_checking(other) => Guard::TypeChecking,
        _ => Guard::Other,
    }
}

/// Whether `expr` names `TYPE_CHECKING`, bare or through a module (`typing.`).
fn is_type_checking(expr: &Expr) -> bool {
    const NAME: &str = "TYPE_CHECKING";
    match expr {
        Expr::Name(name) => name.id.as_str() == NAME,
        Expr::Attribute(attribute) => attribute.attr.as_str() == NAME,
        _ => false,
    }
}
