//! Signature-companion matching between declarations and their companion types.

use std::collections::BTreeSet;

use smol_str::SmolStr;
use swc_ecma_ast::{ClassMember, PropName};

use super::super::references::signature_type_names;

pub(super) fn function_signature_companions(
    owner_name: &str,
    function: &swc_ecma_ast::Function,
) -> Vec<SmolStr> {
    signature_type_names(function)
        .into_iter()
        .filter(|type_name| companion_name_matches(type_name, owner_name))
        .collect()
}

pub(super) fn class_signature_companions(
    class_name: &str,
    class: &swc_ecma_ast::Class,
) -> Vec<SmolStr> {
    let mut companions = Vec::new();
    for member in &class.body {
        let ClassMember::Method(method) = member else {
            continue;
        };
        let PropName::Ident(method_name) = &method.key else {
            continue;
        };
        let owner_name = format!("{class_name}_{}", method_name.sym);
        companions.extend(
            signature_type_names(&method.function)
                .into_iter()
                .filter(|type_name| companion_name_matches(type_name, &owner_name)),
        );
    }
    companions
}

fn companion_name_matches(type_name: &str, owner_name: &str) -> bool {
    const SUFFIXES: [&str; 7] = [
        "Params", "Options", "Input", "Output", "Result", "Context", "State",
    ];
    let Some(stem) = SUFFIXES
        .iter()
        .find_map(|suffix| type_name.strip_suffix(suffix))
    else {
        return false;
    };
    let companion = semantic_tokens(stem);
    companion.len() >= 2 && companion == semantic_tokens(owner_name)
}

fn semantic_tokens(name: &str) -> BTreeSet<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (index, ch) in chars.iter().copied().enumerate() {
        let boundary = !current.is_empty()
            && (ch == '_'
                || ch == '-'
                || (ch.is_uppercase()
                    && chars
                        .get(index.wrapping_sub(1))
                        .is_some_and(|previous| previous.is_lowercase())));
        if boundary {
            words.push(std::mem::take(&mut current));
        }
        if ch != '_' && ch != '-' {
            current.push(ch.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
        .into_iter()
        .filter(|word| word != "adapter" && word != "to")
        .map(|word| normalize_ing(&word))
        .collect()
}

fn normalize_ing(word: &str) -> String {
    let Some(stem) = word.strip_suffix("ing") else {
        return word.to_owned();
    };
    let mut normalized = stem.to_owned();
    if matches!(normalized.as_bytes(), [.., penultimate, last] if penultimate == last) {
        normalized.pop();
    }
    normalized
}
