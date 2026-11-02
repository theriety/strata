//! Visitor implementation that records references, calls, and dynamic imports.

use smol_str::SmolStr;
use swc_ecma_ast::{CallExpr, Callee, Expr, Lit, NewExpr};
use swc_ecma_visit::{Visit, VisitWith};

use super::super::specifier;
use super::collector::{InferBindingCollector, ReferenceCollector};

impl Visit for ReferenceCollector {
    fn visit_ident(&mut self, ident: &swc_ecma_ast::Ident) {
        if self.is_declaration_member_scope
            && self
                .bound_type_names
                .iter()
                .any(|name| name == ident.sym.as_str())
        {
            return;
        }
        self.referenced.push(SmolStr::new(ident.sym.as_str()));
    }

    fn visit_binding_ident(&mut self, binding: &swc_ecma_ast::BindingIdent) {
        if self.is_declaration_member_scope {
            binding.type_ann.visit_with(self);
        } else {
            binding.visit_children_with(self);
        }
    }

    fn visit_ts_type_param(&mut self, param: &swc_ecma_ast::TsTypeParam) {
        if self.is_declaration_member_scope {
            param.constraint.visit_with(self);
            param.default.visit_with(self);
        } else {
            param.visit_children_with(self);
        }
    }

    fn visit_ts_property_signature(&mut self, property: &swc_ecma_ast::TsPropertySignature) {
        if property.computed || !self.is_declaration_member_scope {
            property.key.visit_with(self);
        }
        property.type_ann.visit_with(self);
    }

    fn visit_ts_getter_signature(&mut self, getter: &swc_ecma_ast::TsGetterSignature) {
        if getter.computed || !self.is_declaration_member_scope {
            getter.key.visit_with(self);
        }
        getter.type_ann.visit_with(self);
    }

    fn visit_ts_setter_signature(&mut self, setter: &swc_ecma_ast::TsSetterSignature) {
        if setter.computed || !self.is_declaration_member_scope {
            setter.key.visit_with(self);
        }
        setter.param.visit_with(self);
    }

    fn visit_ts_method_signature(&mut self, method: &swc_ecma_ast::TsMethodSignature) {
        if !self.is_declaration_member_scope {
            method.visit_children_with(self);
            return;
        }
        if method.computed {
            method.key.visit_with(self);
        }
        self.visit_type_parameter_scope(method.type_params.as_deref(), |references| {
            method.params.visit_with(references);
            method.type_ann.visit_with(references);
        });
    }

    fn visit_ts_call_signature_decl(&mut self, call: &swc_ecma_ast::TsCallSignatureDecl) {
        if !self.is_declaration_member_scope {
            call.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(call.type_params.as_deref(), |references| {
            call.params.visit_with(references);
            call.type_ann.visit_with(references);
        });
    }

    fn visit_ts_construct_signature_decl(
        &mut self,
        constructor: &swc_ecma_ast::TsConstructSignatureDecl,
    ) {
        if !self.is_declaration_member_scope {
            constructor.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(constructor.type_params.as_deref(), |references| {
            constructor.params.visit_with(references);
            constructor.type_ann.visit_with(references);
        });
    }

    fn visit_ts_fn_type(&mut self, function: &swc_ecma_ast::TsFnType) {
        if !self.is_declaration_member_scope {
            function.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(function.type_params.as_deref(), |references| {
            function.params.visit_with(references);
            function.type_ann.visit_with(references);
        });
    }

    fn visit_ts_constructor_type(&mut self, constructor: &swc_ecma_ast::TsConstructorType) {
        if !self.is_declaration_member_scope {
            constructor.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(constructor.type_params.as_deref(), |references| {
            constructor.params.visit_with(references);
            constructor.type_ann.visit_with(references);
        });
    }

    fn visit_ts_tuple_element(&mut self, element: &swc_ecma_ast::TsTupleElement) {
        if self.is_declaration_member_scope {
            element.ty.visit_with(self);
        } else {
            element.visit_children_with(self);
        }
    }

    fn visit_ts_type_predicate(&mut self, predicate: &swc_ecma_ast::TsTypePredicate) {
        if self.is_declaration_member_scope {
            predicate.type_ann.visit_with(self);
        } else {
            predicate.visit_children_with(self);
        }
    }

    fn visit_ts_mapped_type(&mut self, mapped: &swc_ecma_ast::TsMappedType) {
        if !self.is_declaration_member_scope {
            mapped.visit_children_with(self);
            return;
        }
        mapped.type_param.constraint.visit_with(self);
        mapped.type_param.default.visit_with(self);
        let name = SmolStr::new(mapped.type_param.name.sym.as_str());
        self.visit_bound_type_scope([name], |references| {
            mapped.name_type.visit_with(references);
            mapped.type_ann.visit_with(references);
        });
    }

    fn visit_ts_conditional_type(&mut self, conditional: &swc_ecma_ast::TsConditionalType) {
        if !self.is_declaration_member_scope {
            conditional.visit_children_with(self);
            return;
        }
        conditional.check_type.visit_with(self);
        let mut bindings = InferBindingCollector::default();
        conditional.extends_type.visit_with(&mut bindings);
        self.visit_bound_type_scope(bindings.names, |references| {
            conditional.extends_type.visit_with(references);
            conditional.true_type.visit_with(references);
        });
        conditional.false_type.visit_with(self);
    }

    fn visit_ts_qualified_name(&mut self, name: &swc_ecma_ast::TsQualifiedName) {
        if self.is_declaration_member_scope {
            name.left.visit_with(self);
        } else {
            name.visit_children_with(self);
        }
    }

    fn visit_object_pat_prop(&mut self, property: &swc_ecma_ast::ObjectPatProp) {
        if !self.is_declaration_member_scope {
            property.visit_children_with(self);
            return;
        }
        match property {
            swc_ecma_ast::ObjectPatProp::KeyValue(property) => {
                if let swc_ecma_ast::PropName::Computed(key) = &property.key {
                    key.expr.visit_with(self);
                }
                property.value.visit_with(self);
            }
            swc_ecma_ast::ObjectPatProp::Assign(property) => {
                property.key.type_ann.visit_with(self);
                property.value.visit_with(self);
            }
            swc_ecma_ast::ObjectPatProp::Rest(rest) => rest.visit_with(self),
        }
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Import(_) => {
                if let Some(first) = call.args.first()
                    && let Expr::Lit(Lit::Str(literal)) = first.expr.as_ref()
                {
                    self.dynamic_imports.push(specifier(literal));
                }
            }
            Callee::Expr(expr) => {
                if let Expr::Ident(ident) = expr.as_ref() {
                    self.called.push(SmolStr::new(ident.sym.as_str()));
                }
            }
            Callee::Super(_) => {}
        }
        call.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        if let Expr::Ident(ident) = new.callee.as_ref() {
            self.called.push(SmolStr::new(ident.sym.as_str()));
        }
        new.visit_children_with(self);
    }
}

impl Visit for InferBindingCollector {
    fn visit_ts_infer_type(&mut self, infer: &swc_ecma_ast::TsInferType) {
        self.names
            .push(SmolStr::new(infer.type_param.name.sym.as_str()));
        infer.type_param.constraint.visit_with(self);
        infer.type_param.default.visit_with(self);
    }
}
