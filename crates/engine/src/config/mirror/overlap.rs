//! Overlap detection between two mirror templates, by exploring their path-part automata.

use super::{MirrorTemplate, match_stem_segment};

impl MirrorTemplate {
    fn parts(&self) -> Vec<PathPart<'_>> {
        self.before_dir
            .iter()
            .map(|segment| PathPart::Literal(segment))
            .chain(std::iter::once(PathPart::Directories))
            .chain(
                self.after_dir
                    .iter()
                    .map(|segment| PathPart::Literal(segment)),
            )
            .chain(std::iter::once(PathPart::Stem {
                prefix: &self.stem_prefix,
                suffix: &self.stem_suffix,
            }))
            .collect()
    }
}

#[derive(Clone, Copy)]
enum PathPart<'a> {
    Literal(&'a str),
    Directories,
    Stem { prefix: &'a str, suffix: &'a str },
}

pub(in crate::config) fn mirror_templates_overlap(
    left: &MirrorTemplate,
    right: &MirrorTemplate,
) -> bool {
    let left = left.parts();
    let right = right.parts();
    let mut pending = vec![(0_usize, 0_usize)];
    let mut visited = std::collections::BTreeSet::new();
    while let Some((left_index, right_index)) = pending.pop() {
        if !visited.insert((left_index, right_index)) {
            continue;
        }
        if left_index == left.len() && right_index == right.len() {
            return true;
        }
        if matches!(left.get(left_index), Some(PathPart::Directories)) {
            pending.push((left_index + 1, right_index));
        }
        if matches!(right.get(right_index), Some(PathPart::Directories)) {
            pending.push((left_index, right_index + 1));
        }
        let (Some(left_part), Some(right_part)) = (left.get(left_index), right.get(right_index))
        else {
            continue;
        };
        if path_parts_intersect(*left_part, *right_part) {
            pending.push((
                left_index + usize::from(!matches!(left_part, PathPart::Directories)),
                right_index + usize::from(!matches!(right_part, PathPart::Directories)),
            ));
        }
    }
    false
}

fn path_parts_intersect(left: PathPart<'_>, right: PathPart<'_>) -> bool {
    match (left, right) {
        (PathPart::Directories, _) | (_, PathPart::Directories) => true,
        (PathPart::Literal(left), PathPart::Literal(right)) => left == right,
        (PathPart::Literal(literal), PathPart::Stem { prefix, suffix })
        | (PathPart::Stem { prefix, suffix }, PathPart::Literal(literal)) => {
            match_stem_segment(literal, prefix, suffix).is_some()
        }
        (
            PathPart::Stem {
                prefix: left_prefix,
                suffix: left_suffix,
            },
            PathPart::Stem {
                prefix: right_prefix,
                suffix: right_suffix,
            },
        ) => stem_patterns_overlap(left_prefix, left_suffix, right_prefix, right_suffix),
    }
}

#[derive(Clone, Copy)]
enum CharacterPart {
    Literal(char),
    Any,
    AnySuffix,
}

fn stem_patterns_overlap(
    left_prefix: &str,
    left_suffix: &str,
    right_prefix: &str,
    right_suffix: &str,
) -> bool {
    fn parts(prefix: &str, suffix: &str) -> Vec<CharacterPart> {
        prefix
            .chars()
            .map(CharacterPart::Literal)
            .chain([CharacterPart::Any, CharacterPart::AnySuffix])
            .chain(suffix.chars().map(CharacterPart::Literal))
            .collect()
    }

    let left = parts(left_prefix, left_suffix);
    let right = parts(right_prefix, right_suffix);
    let mut pending = vec![(0_usize, 0_usize)];
    let mut visited = std::collections::BTreeSet::new();
    while let Some((left_index, right_index)) = pending.pop() {
        if !visited.insert((left_index, right_index)) {
            continue;
        }
        if left_index == left.len() && right_index == right.len() {
            return true;
        }
        if matches!(left.get(left_index), Some(CharacterPart::AnySuffix)) {
            pending.push((left_index + 1, right_index));
        }
        if matches!(right.get(right_index), Some(CharacterPart::AnySuffix)) {
            pending.push((left_index, right_index + 1));
        }
        let (Some(left_part), Some(right_part)) = (left.get(left_index), right.get(right_index))
        else {
            continue;
        };
        if character_parts_intersect(*left_part, *right_part) {
            pending.push((
                left_index + usize::from(!matches!(left_part, CharacterPart::AnySuffix)),
                right_index + usize::from(!matches!(right_part, CharacterPart::AnySuffix)),
            ));
        }
    }
    false
}

fn character_parts_intersect(left: CharacterPart, right: CharacterPart) -> bool {
    match (left, right) {
        (CharacterPart::Literal(left), CharacterPart::Literal(right)) => left == right,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_distinguish_literal_segments_that_do_and_do_not_match_a_stem() {
        let stem = PathPart::Stem {
            prefix: "pre-",
            suffix: ".ts",
        };

        assert!(path_parts_intersect(PathPart::Literal("pre-item.ts"), stem));
        assert!(path_parts_intersect(stem, PathPart::Literal("pre-item.ts")));
        assert!(!path_parts_intersect(PathPart::Literal("item.ts"), stem));
    }
}
