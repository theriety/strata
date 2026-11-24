//! Runtime module loads stated by top-level import and re-export statements.

use swc_common::{BytePos, Span, Spanned};
use swc_ecma_ast::{ExportSpecifier, ImportSpecifier, ModuleDecl, TsModuleRef};
use unlace_ir::ImportKind;

use crate::parse::{ModuleLoad, specifier};

/// The runtime load `decl` states, or `None` when it loads nothing at runtime:
/// a declaration that is not an import or re-export, a statement written
/// `type`, or one whose every specifier is `type`-qualified.
pub(super) fn module_load(decl: &ModuleDecl, contents: &str, base: BytePos) -> Option<ModuleLoad> {
    let (source, kind) = match decl {
        ModuleDecl::Import(import) if !import.type_only => {
            (specifier(&import.src), import_kind(&import.specifiers)?)
        }
        ModuleDecl::ExportNamed(named) if !named.type_only => {
            let src = named.src.as_deref()?;
            (specifier(src), re_export_kind(&named.specifiers)?)
        }
        ModuleDecl::ExportAll(all) if !all.type_only => (specifier(&all.src), ImportKind::Value),
        ModuleDecl::TsImportEquals(equals) if !equals.is_type_only => {
            let TsModuleRef::TsExternalModuleRef(external) = &equals.module_ref else {
                return None;
            };
            (specifier(&external.expr), ImportKind::Value)
        }
        _ => return None,
    };
    Some(ModuleLoad {
        source,
        kind,
        line: line_of(decl.span(), contents, base),
    })
}

/// `import './x'` runs the module, `import { type A } from './x'` loads
/// nothing, and any other import binds a runtime name.
fn import_kind(specifiers: &[ImportSpecifier]) -> Option<ImportKind> {
    if specifiers.is_empty() {
        return Some(ImportKind::SideEffect);
    }
    specifiers
        .iter()
        .any(|specifier| !matches!(specifier, ImportSpecifier::Named(named) if named.is_type_only))
        .then_some(ImportKind::Value)
}

/// `export { type A } from './x'` loads nothing; any other re-export binds.
fn re_export_kind(specifiers: &[ExportSpecifier]) -> Option<ImportKind> {
    (specifiers.is_empty()
        || specifiers.iter().any(
            |specifier| !matches!(specifier, ExportSpecifier::Named(named) if named.is_type_only),
        ))
    .then_some(ImportKind::Value)
}

/// The 1-based line a span starts on.
fn line_of(span: Span, contents: &str, base: BytePos) -> u32 {
    let offset = (span.lo.0 - base.0) as usize;
    let newlines = contents
        .get(..offset)
        .map_or(0, |head| head.bytes().filter(|byte| *byte == b'\n').count());
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}
