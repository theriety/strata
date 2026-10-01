//! Statement walker: feeds the child expressions and bodies of a statement.

use std::collections::HashSet;

use rustpython_parser::ast::Stmt;
use smol_str::SmolStr;

use super::collector::{ReferenceCollector, collect_body};
use crate::scope::pattern_names;

/// Walks the direct child expressions of a statement into the collector.
pub(super) fn collect_child_expressions(statement: &Stmt, collector: &mut ReferenceCollector) {
    match statement {
        Stmt::Return(statement) => {
            if let Some(value) = &statement.value {
                collector.collect_expression(value);
            }
        }
        Stmt::Assign(assign) => collector.collect_expression(&assign.value),
        Stmt::AugAssign(assign) => collector.collect_expression(&assign.value),
        Stmt::Expr(statement) => collector.collect_expression(&statement.value),
        Stmt::If(statement) => {
            collector.collect_expression(&statement.test);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::While(statement) => {
            collector.collect_expression(&statement.test);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::For(statement) => {
            collector.collect_expression(&statement.iter);
            collector.bind_target(&statement.target);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::AsyncFor(statement) => {
            collector.collect_expression(&statement.iter);
            collector.bind_target(&statement.target);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::With(statement) => {
            for item in &statement.items {
                collector.collect_expression(&item.context_expr);
                if let Some(vars) = &item.optional_vars {
                    collector.bind_target(vars);
                }
            }
            collect_body(&statement.body, collector);
        }
        Stmt::AsyncWith(statement) => {
            for item in &statement.items {
                collector.collect_expression(&item.context_expr);
                if let Some(vars) = &item.optional_vars {
                    collector.bind_target(vars);
                }
            }
            collect_body(&statement.body, collector);
        }
        Stmt::Match(statement) => {
            collector.collect_expression(&statement.subject);
            for case in &statement.cases {
                let mut captures = HashSet::new();
                pattern_names(&case.pattern, &mut captures);
                collector.bind_locals(captures);
                if let Some(guard) = &case.guard {
                    collector.collect_expression(guard);
                }
                collect_body(&case.body, collector);
            }
        }
        Stmt::Raise(statement) => {
            if let Some(exception) = &statement.exc {
                collector.collect_expression(exception);
            }
        }
        Stmt::Assert(statement) => {
            collector.collect_expression(&statement.test);
            if let Some(message) = &statement.msg {
                collector.collect_expression(message);
            }
        }
        Stmt::Try(statement) => {
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
            collect_body(&statement.finalbody, collector);
            for handler in &statement.handlers {
                let rustpython_parser::ast::ExceptHandler::ExceptHandler(handler) = handler;
                if let Some(name) = &handler.name {
                    collector.bind_locals(HashSet::from([SmolStr::new(name.as_str())]));
                }
                collect_body(&handler.body, collector);
            }
        }
        Stmt::TryStar(statement) => {
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
            collect_body(&statement.finalbody, collector);
            for handler in &statement.handlers {
                let rustpython_parser::ast::ExceptHandler::ExceptHandler(handler) = handler;
                if let Some(name) = &handler.name {
                    collector.bind_locals(HashSet::from([SmolStr::new(name.as_str())]));
                }
                collect_body(&handler.body, collector);
            }
        }
        _ => {}
    }
}
