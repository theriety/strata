//! The one path form the result boundary rewrites: group-keyed paths.

/// The real name of the repository root directory.
const ROOT: &str = ".";

/// A path field the analysis keys under its package-group label
/// (`group/hub/a.py`) until the result boundary rewrites it to a real
/// repo-relative path.
///
/// The group name is a label, not a directory, so the keyed form never
/// serializes. A record hands out this view only for the fields that really are
/// keyed; a path that was real from the start (a symbol move's) offers none, so
/// rewriting one cannot be written. In a flat layout (`proj/proj/a.py`) a real
/// path may begin with the group's own name, which is why the type, not the
/// string, decides what is rewritten.
pub(crate) struct GroupKeyed<'a>(&'a mut String);

impl<'a> GroupKeyed<'a> {
    /// Views `path`, which the calling record knows to be group-keyed.
    pub(super) fn new(path: &'a mut String) -> Self {
        Self(path)
    }

    /// Rewrites the path to its real form: the group itself is the repository
    /// root, and one leading `group/` segment is dropped from anything below it.
    pub(crate) fn into_real(self, group: &str) {
        if self.0.as_str() == group {
            ROOT.clone_into(self.0);
        } else if self
            .0
            .strip_prefix(group)
            .is_some_and(|rest| rest.starts_with('/'))
        {
            self.0.drain(..=group.len());
        }
    }
}
