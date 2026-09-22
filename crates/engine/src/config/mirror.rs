use crate::error::StrataError;

#[derive(Debug, Clone, Default)]
pub(crate) struct MirrorTemplate {
    before_dir: Vec<String>,
    after_dir: Vec<String>,
    stem_prefix: String,
    stem_suffix: String,
}

#[derive(Debug, Clone)]
pub(crate) struct MirrorCaptures {
    pub(crate) dir: String,
    pub(crate) stem: String,
}

impl MirrorTemplate {
    pub(crate) fn parse(template: &str) -> Option<Self> {
        let segments = template.split('/').collect::<Vec<_>>();
        if segments.iter().any(|segment| segment.is_empty()) {
            return None;
        }
        let dir_index = segments.iter().position(|segment| *segment == "{dir}")?;
        if segments
            .iter()
            .enumerate()
            .any(|(index, segment)| index != dir_index && segment.contains("{dir}"))
        {
            return None;
        }
        let stem_segment = *segments.last()?;
        let (stem_prefix, stem_suffix) = stem_segment.split_once("{stem}")?;
        if dir_index >= segments.len().saturating_sub(1)
            || stem_prefix.contains(['{', '}'])
            || stem_suffix.contains(['{', '}'])
            || segments
                .get(..segments.len().saturating_sub(1))?
                .iter()
                .enumerate()
                .any(|(index, segment)| {
                    index != dir_index
                        && (segment.contains("{stem}") || segment.contains(['{', '}']))
                })
        {
            return None;
        }
        Some(Self {
            before_dir: segments
                .get(..dir_index)?
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
            after_dir: segments
                .get(dir_index + 1..segments.len().saturating_sub(1))?
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
            stem_prefix: stem_prefix.to_owned(),
            stem_suffix: stem_suffix.to_owned(),
        })
    }

    pub(crate) fn captures(&self, path: &str) -> Option<MirrorCaptures> {
        let segments = path.split('/').collect::<Vec<_>>();
        let fixed = self.before_dir.len() + self.after_dir.len() + 1;
        if segments.len() < fixed || segments.iter().any(|segment| segment.is_empty()) {
            return None;
        }
        if segments
            .get(..self.before_dir.len())?
            .iter()
            .copied()
            .ne(self.before_dir.iter().map(String::as_str))
        {
            return None;
        }
        let stem_index = segments.len() - 1;
        let after_start = stem_index.checked_sub(self.after_dir.len())?;
        if after_start < self.before_dir.len()
            || segments
                .get(after_start..stem_index)?
                .iter()
                .copied()
                .ne(self.after_dir.iter().map(String::as_str))
        {
            return None;
        }
        let stem = match_stem_segment(
            segments.get(stem_index)?,
            &self.stem_prefix,
            &self.stem_suffix,
        )?;
        Some(MirrorCaptures {
            dir: segments.get(self.before_dir.len()..after_start)?.join("/"),
            stem: stem.to_owned(),
        })
    }

    pub(crate) fn fill(&self, dir: &str, stem: &str) -> String {
        let mut segments = self.before_dir.clone();
        segments.extend(
            dir.split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned),
        );
        segments.extend(self.after_dir.iter().cloned());
        segments.push(format!("{}{}{}", self.stem_prefix, stem, self.stem_suffix));
        segments.join("/")
    }

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

fn match_stem_segment<'a>(segment: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let stem = segment.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!stem.is_empty() && !stem.contains('/')).then_some(stem)
}

pub(super) fn mirror_templates_overlap(left: &MirrorTemplate, right: &MirrorTemplate) -> bool {
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

pub(super) fn validate_mirror_template(template: &str, key: &str) -> Result<(), StrataError> {
    if template.is_empty()
        || template.starts_with('/')
        || template.split('/').any(|part| part == "..")
    {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "mirror templates must be non-empty repo-relative paths".to_owned(),
        });
    }
    let mut rest = template;
    let mut placeholders = Vec::new();
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(StrataError::ConfigInvalid {
                key: Some(key.to_owned()),
                reason: "unterminated mirror placeholder".to_owned(),
            });
        };
        let placeholder = &after[..end];
        if !matches!(placeholder, "dir" | "stem") {
            return Err(StrataError::ConfigInvalid {
                key: Some(key.to_owned()),
                reason: "unknown mirror placeholder; expected {dir} or {stem}".to_owned(),
            });
        }
        placeholders.push(placeholder);
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "unmatched mirror placeholder terminator".to_owned(),
        });
    }
    if placeholders != ["dir", "stem"] {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "mirror templates require exactly one {dir} followed by exactly one {stem}"
                .to_owned(),
        });
    }
    if MirrorTemplate::parse(template).is_none() {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "mirror template grammar requires {dir} as a complete path segment and {stem} in the final filename segment"
                .to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror_template(template: &str) -> MirrorTemplate {
        let parsed = MirrorTemplate::parse(template);
        assert!(
            parsed.is_some(),
            "test mirror template must parse: {template}"
        );
        parsed.unwrap_or_default()
    }

    #[test]
    fn should_detect_overlap_when_stem_captures_have_unequal_lengths() {
        let prefixed = mirror_template("root/{dir}/pre-{stem}.ts");
        let suffixed = mirror_template("root/{dir}/{stem}-post.ts");

        assert!(mirror_templates_overlap(&prefixed, &suffixed));
        assert!(mirror_templates_overlap(&suffixed, &prefixed));
    }

    #[test]
    fn should_capture_zero_single_and_multiple_directory_segments() {
        let template = mirror_template("root/{dir}/item-{stem}.ts");

        for (path, expected_dir) in [
            ("root/item-alpha.ts", ""),
            ("root/one/item-alpha.ts", "one"),
            ("root/one/two/item-alpha.ts", "one/two"),
        ] {
            let captures = template.captures(path);
            assert_eq!(
                captures.as_ref().map(|value| value.dir.as_str()),
                Some(expected_dir)
            );
            assert_eq!(
                captures.as_ref().map(|value| value.stem.as_str()),
                Some("alpha")
            );
        }
    }

    #[test]
    fn should_intersect_directory_languages_at_zero_single_and_multiple_segments() {
        for right in [
            "root/{dir}/{stem}.ts",
            "root/one/{dir}/{stem}.ts",
            "root/one/two/{dir}/{stem}.ts",
        ] {
            let left = mirror_template("root/{dir}/{stem}.ts");
            let right = mirror_template(right);
            assert!(mirror_templates_overlap(&left, &right));
            assert!(mirror_templates_overlap(&right, &left));
        }
    }

    #[test]
    fn should_distinguish_intersecting_and_disjoint_stem_affixes() {
        let broad = mirror_template("root/{dir}/pre-{stem}-post.ts");
        let intersecting = mirror_template("root/{dir}/pre-x{stem}-post.ts");
        let disjoint = mirror_template("root/{dir}/other-{stem}-post.ts");

        assert!(mirror_templates_overlap(&broad, &intersecting));
        assert!(!mirror_templates_overlap(&broad, &disjoint));
    }

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

    #[test]
    fn should_compute_template_overlap_symmetrically_and_deterministically() {
        let templates = [
            mirror_template("root/{dir}/{stem}.ts"),
            mirror_template("root/fixed/{dir}/pre-{stem}.ts"),
            mirror_template("other/{dir}/{stem}-post.ts"),
        ];

        for left in &templates {
            for right in &templates {
                let expected = mirror_templates_overlap(left, right);
                assert_eq!(mirror_templates_overlap(right, left), expected);
                assert_eq!(mirror_templates_overlap(left, right), expected);
            }
        }
    }
}
