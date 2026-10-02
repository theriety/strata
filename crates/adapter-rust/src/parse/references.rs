//! Reference collection over declaration subtrees.
//!
//! A syn visitor gathers use paths, calls, type positions, and qualifiers with
//! the byte offset at which each occurs, tracking lexical bindings so locals are
//! not mistaken for items and macro context so edges can be marked uncertain.

use smol_str::SmolStr;
use syn::visit::Visit;

use super::declaration::byte_offset;
use super::{RefKind, Reference};

/// Visits a declaration subtree, collecting every reference of interest with its
/// source byte offset.
pub(super) fn collect_references<'ast, V>(node: &'ast V) -> Vec<Reference>
where
    ReferenceCollector: Visit<'ast>,
    V: VisitNode,
{
    let mut collector = ReferenceCollector::default();
    node.accept(&mut collector);
    collector.references
}

/// A node a [`ReferenceCollector`] can be driven over.
pub(super) trait VisitNode {
    /// Drives `collector` over `self`.
    fn accept<'ast>(&'ast self, collector: &mut ReferenceCollector)
    where
        ReferenceCollector: Visit<'ast>;
}

/// Generates the [`VisitNode`] impl for each visited syn item type.
macro_rules! impl_visit_node {
    ($($ty:ty => $method:ident),+ $(,)?) => {
        $(
            impl VisitNode for $ty {
                fn accept<'ast>(&'ast self, collector: &mut ReferenceCollector)
                where
                    ReferenceCollector: Visit<'ast>,
                {
                    collector.$method(self);
                }
            }
        )+
    };
}

impl_visit_node! {
    syn::ItemFn => visit_item_fn,
    syn::ItemStruct => visit_item_struct,
    syn::ItemEnum => visit_item_enum,
    syn::ItemUnion => visit_item_union,
    syn::ItemTrait => visit_item_trait,
    syn::ItemType => visit_item_type,
    syn::ItemConst => visit_item_const,
    syn::ItemStatic => visit_item_static,
    syn::ImplItemFn => visit_impl_item_fn,
    syn::ImplItem => visit_impl_item,
}

/// A syn visitor that gathers references (use paths, calls, type positions) with
/// the byte offset at which each occurs, tracking whether the cursor is inside a
/// macro invocation so resolved edges can be marked lower-confidence.
#[derive(Default)]
pub(super) struct ReferenceCollector {
    /// The references gathered so far, in source order.
    references: Vec<Reference>,
    /// Macro-invocation nesting depth; non-zero marks macro-expanded context.
    macro_depth: u32,
    /// Names bound in the lexical scopes enclosing the cursor (parameters, let,
    /// closure parameters, patterns); bare uses of them are locals, not items.
    /// Blocks, closures, match arms, and `if`/`while`/`for` truncate it on exit.
    bound: Vec<String>,
}

impl ReferenceCollector {
    /// Runs `visit`, then forgets every name it bound, so a binding does not
    /// outlive the block, closure, arm, or loop that introduced it.
    fn scoped(&mut self, visit: impl FnOnce(&mut Self)) {
        let depth = self.bound.len();
        visit(self);
        self.bound.truncate(depth);
    }

    /// Records a reference of `kind` to `ident`, stamping the current macro
    /// context onto it. The identifier's text is retained so binding can fall
    /// back to name-based resolution when `goto_definition` comes up empty.
    fn record(&mut self, kind: RefKind, ident: &syn::Ident) {
        self.references.push(Reference {
            kind,
            name: SmolStr::new(ident.to_string()),
            offset: byte_offset(ident.span()),
            macro_expanded: self.macro_depth > 0,
        });
    }
}

impl<'ast> Visit<'ast> for ReferenceCollector {
    fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
        self.bound.push(pat.ident.to_string());
        syn::visit::visit_pat_ident(self, pat);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        // The initializer (and let-else block) is evaluated before the pattern
        // binds, so `let helper = helper;` still reads the outer `helper`.
        for attr in &local.attrs {
            self.visit_attribute(attr);
        }
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.visit_pat(&local.pat);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scoped(|this| syn::visit::visit_block(this, block));
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        self.scoped(|this| syn::visit::visit_expr_closure(this, closure));
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        self.scoped(|this| syn::visit::visit_arm(this, arm));
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        self.scoped(|this| syn::visit::visit_expr_if(this, expr));
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        self.scoped(|this| syn::visit::visit_expr_while(this, expr));
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        self.scoped(|this| syn::visit::visit_expr_for_loop(this, expr));
    }

    fn visit_use_path(&mut self, use_path: &'ast syn::UsePath) {
        self.record(RefKind::UsePath, &use_path.ident);
        syn::visit::visit_use_path(self, use_path);
    }

    fn visit_use_name(&mut self, use_name: &'ast syn::UseName) {
        self.record(RefKind::UsePath, &use_name.ident);
        syn::visit::visit_use_name(self, use_name);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref()
            && let Some(segment) = path.path.segments.last()
        {
            self.record(RefKind::Call, &segment.ident);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.record(RefKind::Call, &call.method);
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_struct(&mut self, expr_struct: &'ast syn::ExprStruct) {
        // A struct literal `Foo { .. }` depends on the constructed type.
        if let Some(segment) = expr_struct.path.segments.last() {
            self.record(RefKind::TypeRef, &segment.ident);
        }
        syn::visit::visit_expr_struct(self, expr_struct);
    }

    fn visit_expr_path(&mut self, expr_path: &'ast syn::ExprPath) {
        // Qualified value paths (`Type::assoc`, `module::ITEM`, `Enum::Variant`)
        // name a cross-item dependency; bare single-segment paths are usually
        // locals, so only multi-segment paths are recorded. Direct call callees
        // are already handled by `visit_expr_call`; duplicates collapse in bind.
        // The qualifier is a dependency too when it names a type: `Type::new()`
        // resolves its leaf to the associated function, so the qualifier is
        // recorded separately and bind keeps it only if it lands on a type.
        let segments = &expr_path.path.segments;
        if segments.len() == 1
            && expr_path.qself.is_none()
            && expr_path.path.leading_colon.is_none()
            && let Some(segment) = segments.first()
        {
            // A bare name is a function used as a value (`.map_or(0, f)`) unless
            // the declaration binds it (let, parameter, closure, pattern) or it
            // is a path keyword. Bind resolves it only through the semantic
            // database, never by name.
            let name = segment.ident.to_string();
            if !matches!(name.as_str(), "self" | "Self" | "crate" | "super")
                && !self.bound.contains(&name)
            {
                self.record(RefKind::Call, &segment.ident);
            }
        }
        if segments.len() > 1
            && let Some(segment) = segments.last()
        {
            self.record(RefKind::Call, &segment.ident);
            if let Some(qualifier) = segments.iter().nth_back(1) {
                self.record(RefKind::Qualifier, &qualifier.ident);
            }
        }
        syn::visit::visit_expr_path(self, expr_path);
    }

    fn visit_type_path(&mut self, type_path: &'ast syn::TypePath) {
        if let Some(segment) = type_path.path.segments.last() {
            self.record(RefKind::TypeRef, &segment.ident);
        }
        syn::visit::visit_type_path(self, type_path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // A macro's token stream is not part of the typed AST, so the default
        // visit never descends into it. Best-effort: re-parse the tokens as a
        // comma-separated expression list (the shape of `println!`, `vec!`,
        // `assert!`, …) and visit any recovered expressions with the macro flag
        // raised, so references that originate inside a macro resolve at reduced
        // confidence rather than being silently dropped.
        self.macro_depth += 1;
        if let Ok(args) = mac.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        ) {
            for expr in &args {
                self.visit_expr(expr);
            }
        }
        self.macro_depth = self.macro_depth.saturating_sub(1);
    }
}
