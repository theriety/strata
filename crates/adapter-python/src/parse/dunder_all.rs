//! `__all__` extraction: the names a module lists and whether it lists them
//! all in a form the parser can read.
//!
//! A module-level `__all__ = [...]` of string literals is a closed list, as long
//! as nothing else in the module mentions `__all__`. Any other mention (an
//! augmented assignment, a method call, a subscript store, a `global`
//! declaration, a `globals()["__all__"]` lookup, a change inside a function or a
//! class body) leaves the list open, because the parser cannot tell what the
//! module finally exports. Mentions are counted from the token stream, so
//! comments and docstrings that talk about `__all__` do not count.

use rustpython_parser::Mode;
use rustpython_parser::Tok;
use rustpython_parser::ast::{Constant, Expr, Stmt};
use rustpython_parser::lexer::lex;
use smol_str::SmolStr;

use super::DunderAllForm;

const DUNDER_ALL: &str = "__all__";

/// The `__all__` of one module.
pub(super) struct DunderAll {
    /// The string literals of the module-level `__all__ = [...]` assignments.
    /// The list is complete only when `form` is [`DunderAllForm::Literal`]; for
    /// any other form it holds what could be read and must not close a surface.
    pub(super) names: Vec<SmolStr>,
    /// Whether `names` is the whole list.
    pub(super) form: DunderAllForm,
}

/// Reads the `__all__` of a module from its top-level statements and its source.
pub(super) fn scan(suite: &[Stmt], source: &str) -> DunderAll {
    let mut names = Vec::new();
    let mut literal_targets = 0_usize;
    let mut unreadable_assignment = false;
    for statement in suite {
        let Stmt::Assign(assign) = statement else {
            continue;
        };
        let targets = assign.targets.iter().filter(|target| is_dunder_all(target));
        let target_count = targets.count();
        if target_count == 0 {
            continue;
        }
        match literal_names(&assign.value) {
            Some(listed) => {
                names.extend(listed);
                literal_targets += target_count;
            }
            None => unreadable_assignment = true,
        }
    }

    let mentions = Mentions::count(source);
    let form = if mentions.names == 0 && !mentions.quoted {
        DunderAllForm::Absent
    } else if unreadable_assignment || mentions.quoted || mentions.names != literal_targets {
        DunderAllForm::Dynamic
    } else {
        DunderAllForm::Literal
    };
    DunderAll { names, form }
}

/// Whether `expression` is the bare name `__all__`.
fn is_dunder_all(expression: &Expr) -> bool {
    matches!(expression, Expr::Name(name) if name.id.as_str() == DUNDER_ALL)
}

/// The string literals of a list or tuple made only of string literals.
fn literal_names(value: &Expr) -> Option<Vec<SmolStr>> {
    let elements = match value {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    elements
        .iter()
        .map(|element| match element {
            Expr::Constant(constant) => match &constant.value {
                Constant::Str(text) => Some(SmolStr::new(text.as_str())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// How often the source mentions `__all__`.
struct Mentions {
    /// The number of `__all__` identifiers, in any position.
    names: usize,
    /// Whether a string literal is exactly `"__all__"` (`globals()["__all__"]`,
    /// `setattr(module, "__all__", ...)`).
    quoted: bool,
}

impl Mentions {
    /// Counts the mentions in the token stream of `source`.
    fn count(source: &str) -> Self {
        let mut mentions = Self {
            names: 0,
            quoted: false,
        };
        for (token, _) in lex(source, Mode::Module).flatten() {
            match token {
                Tok::Name { name } if name == DUNDER_ALL => mentions.names += 1,
                Tok::String { value, .. } if value == DUNDER_ALL => mentions.quoted = true,
                _ => {}
            }
        }
        mentions
    }
}
