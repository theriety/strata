//! Source-like token rendering for trait impl headers.
//!
//! Prints syn nodes with compact spacing so a trait impl's declaration name reads
//! like source (`impl From<Vec<u8>> for Wrapper`) rather than a spaced token dump.

use quote::ToTokens;
use syn::ItemImpl;

/// Renders a trait impl's header without its own generic parameter list:
/// `impl<T> From<T> for Wrapper<T>` becomes `impl From<T> for Wrapper<T>`.
pub(super) fn trait_impl_header(item_impl: &ItemImpl) -> String {
    let (negative, trait_path) = item_impl
        .trait_
        .as_ref()
        .map_or((false, None), |(bang, path, _)| {
            (bang.is_some(), Some(path))
        });
    format!(
        "impl {}{} for {}",
        if negative { "!" } else { "" },
        trait_path.map(compact_tokens).unwrap_or_default(),
        compact_tokens(&item_impl.self_ty)
    )
}

/// Prints `node` as source-like text: tokens are joined without spacing except
/// between two word-like tokens, after `,` and `;`, and around `+` and `->`
/// (`From < Vec < u8 > >` becomes `From<Vec<u8>>`, `* const T` becomes
/// `*const T`).
fn compact_tokens(node: &impl ToTokens) -> String {
    let mut text = String::new();
    write_tokens(node.to_token_stream(), &mut text);
    text
}

/// Appends `stream` to `text`, inserting a space only where two tokens would
/// otherwise fuse or where a separator reads better spaced.
fn write_tokens(stream: proc_macro2::TokenStream, text: &mut String) {
    use proc_macro2::{Delimiter, Spacing, TokenTree};

    let mut previous_is_word = false;
    let mut previous_is_lifetime = false;
    let mut after_tick = false;
    let mut pending_space = false;
    let mut tokens = stream.into_iter().peekable();
    while let Some(token) = tokens.next() {
        let is_word = matches!(token, TokenTree::Ident(_) | TokenTree::Literal(_));
        let lifetime_tick = matches!(&token, TokenTree::Punct(p) if p.as_char() == '\'');
        // `*mut [u8; 4]`: a bracket group after a qualifier is a type, not an index.
        let slice_type = matches!(&token, TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket)
            && matches!(
                text.rsplit([' ', '*', '&']).next(),
                Some("mut" | "const" | "dyn")
            );
        // `for<'a> Fn(&'a u8)` and `&'a [u8]`: a closed higher-ranked binder
        // or a lifetime name must not fuse with the word or group after it.
        let after_binder =
            matches!(token, TokenTree::Ident(_)) && !after_tick && text.ends_with('>');
        let after_lifetime = previous_is_lifetime
            && matches!(&token, TokenTree::Group(g) if g.delimiter() != Delimiter::None);
        if (pending_space
            || slice_type
            || after_binder
            || after_lifetime
            || ((is_word || lifetime_tick) && previous_is_word))
            && !text.is_empty()
        {
            text.push(' ');
        }
        pending_space = false;
        previous_is_lifetime = after_tick && is_word;
        after_tick = lifetime_tick;
        previous_is_word = is_word;
        match token {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Brace => ("{ ", " }"),
                    Delimiter::None => ("", ""),
                };
                if group.stream().is_empty() {
                    text.push_str(open.trim_end());
                    text.push_str(close.trim_start());
                } else {
                    text.push_str(open);
                    write_tokens(group.stream(), text);
                    text.push_str(close);
                }
            }
            TokenTree::Punct(punct) => {
                let ch = punct.as_char();
                let arrow = ch == '-'
                    && punct.spacing() == Spacing::Joint
                    && matches!(tokens.peek(), Some(TokenTree::Punct(next)) if next.as_char() == '>');
                if arrow {
                    tokens.next();
                    text.push_str(" ->");
                    pending_space = true;
                } else if ch == '+' {
                    text.push_str(" +");
                    pending_space = true;
                } else if ch == '='
                    && punct.spacing() == Spacing::Alone
                    && !text.ends_with(['<', '>', '=', '!'])
                {
                    text.push_str(" =");
                    pending_space = true;
                } else {
                    text.push(ch);
                    pending_space = matches!(ch, ',' | ';');
                }
            }
            TokenTree::Ident(ident) => text.push_str(&ident.to_string()),
            TokenTree::Literal(literal) => text.push_str(&literal.to_string()),
        }
    }
}
