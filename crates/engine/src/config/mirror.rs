//! Exact source-to-test mirror templates: parsing, capture filling, overlap, and validation.

mod overlap;
#[cfg(test)]
mod tests;
mod validate;

pub(super) use overlap::mirror_templates_overlap;
pub(super) use validate::validate_mirror_template;

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
}

fn match_stem_segment<'a>(segment: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let stem = segment.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!stem.is_empty() && !stem.contains('/')).then_some(stem)
}
