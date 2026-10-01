//! Declaration construction and byte-range helpers.
//!
//! Builds a [`Declaration`] from its parts, computing the byte range, production
//! SLOC, and visibility metadata, and converts spans to saturating `u32` offsets.

use proc_macro2::Span;
use smol_str::SmolStr;

use super::metadata::{is_public, visibility};
use super::{DeclKind, Declaration, Reference};
use crate::sloc::production_sloc;

/// Builds a [`Declaration`] from its parts, computing byte range and SLOC.
pub(super) fn declaration(
    name: &str,
    kind: DeclKind,
    source_visibility: &syn::Visibility,
    cfg_test: bool,
    span: Span,
    contents: &str,
    references: Vec<Reference>,
) -> Declaration {
    let (visibility_kind, visibility_path) = visibility(source_visibility);
    let byte_start = byte_start(span);
    let byte_end = byte_end(span);
    let sloc = if cfg_test {
        // `cfg(test)` regions are excluded from production SLOC.
        0
    } else {
        slice_sloc(byte_start, byte_end, contents)
    };
    Declaration {
        name: SmolStr::new(name),
        kind,
        exported: is_public(source_visibility),
        visibility_kind,
        visibility_path,
        cfg_test,
        test_case: false,
        byte_start,
        byte_end,
        sloc,
        references,
    }
}

/// Computes production SLOC for the byte slice `[start, end)` of `contents`.
fn slice_sloc(start: u32, end: u32, contents: &str) -> u32 {
    contents
        .get(start as usize..end as usize)
        .map_or(0, production_sloc)
}

/// 0-based byte offset of a span's start, saturating into `u32`.
fn byte_start(span: Span) -> u32 {
    byte_offset(span)
}

/// 0-based byte offset of a span's start, saturating into `u32`.
pub(super) fn byte_offset(span: Span) -> u32 {
    u32::try_from(span.byte_range().start).unwrap_or(u32::MAX)
}

/// 0-based byte offset one past a span's end, saturating into `u32`.
fn byte_end(span: Span) -> u32 {
    u32::try_from(span.byte_range().end).unwrap_or(u32::MAX)
}
