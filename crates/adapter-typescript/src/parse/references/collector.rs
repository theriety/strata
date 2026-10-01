//! Reference collector state and scope handling for declaration subtrees.

use smol_str::SmolStr;
use swc_ecma_visit::VisitWith;

use super::References;

/// A [`Visit`] that gathers referenced identifiers, invoked identifiers, and
/// dynamic-import literals reachable from a single top-level declaration's body.
#[derive(Default)]
pub(super) struct ReferenceCollector {
    /// Whether declaration-member keys and binders should be excluded.
    pub(super) is_declaration_member_scope: bool,
    /// Type-level names bound by the declaration scopes currently being visited.
    pub(super) bound_type_names: Vec<SmolStr>,
    /// Identifiers referenced within the visited subtree.
    pub(super) referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call or `new` target within the subtree.
    pub(super) called: Vec<SmolStr>,
    /// Literal specifiers of `import('...')` calls within the subtree.
    pub(super) dynamic_imports: Vec<SmolStr>,
}

impl ReferenceCollector {
    pub(super) fn for_declaration_members() -> Self {
        Self {
            is_declaration_member_scope: true,
            ..Self::default()
        }
    }

    pub(super) fn finish(self) -> References {
        References {
            referenced: self.referenced,
            called: self.called,
            dynamic_imports: self.dynamic_imports,
        }
    }

    pub(super) fn visit_type_parameter_scope(
        &mut self,
        params: Option<&swc_ecma_ast::TsTypeParamDecl>,
        visit: impl FnOnce(&mut Self),
    ) {
        let Some(params) = params else {
            visit(self);
            return;
        };
        let names = params
            .params
            .iter()
            .map(|param| SmolStr::new(param.name.sym.as_str()));
        self.visit_bound_type_scope(names, |references| {
            for param in &params.params {
                param.constraint.visit_with(references);
                param.default.visit_with(references);
            }
            visit(references);
        });
    }

    pub(super) fn visit_bound_type_scope(
        &mut self,
        names: impl IntoIterator<Item = SmolStr>,
        visit: impl FnOnce(&mut Self),
    ) {
        let mut scoped = Self {
            is_declaration_member_scope: self.is_declaration_member_scope,
            bound_type_names: self.bound_type_names.clone(),
            ..Self::default()
        };
        scoped.bound_type_names.extend(names);
        visit(&mut scoped);
        self.referenced.extend(scoped.referenced);
        self.called.extend(scoped.called);
        self.dynamic_imports.extend(scoped.dynamic_imports);
    }
}

#[derive(Default)]
pub(super) struct InferBindingCollector {
    pub(super) names: Vec<SmolStr>,
}
