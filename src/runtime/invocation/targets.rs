use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::infra::terraform::discovery::DEFAULT_MAX_DEPTH;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnvironmentTargets {
    directories: Vec<PathBuf>,
    max_depth: Option<usize>,
}

impl EnvironmentTargets {
    // Only arguments before the command word are read, so forwarded arguments never match.
    /// # Errors
    ///
    /// Returns a usage message when an option lacks a value or the values conflict.
    pub fn parse_leading(arguments: &[OsString]) -> Result<(Self, usize), String> {
        let mut targets = Self::default();
        let mut index = 0;
        while let Some(argument) = arguments.get(index).and_then(|argument| argument.to_str()) {
            let (name, inline) = argument
                .split_once('=')
                .map_or((argument, None), |(name, value)| (name, Some(value)));
            if !matches!(name, "--env-dir" | "--max-depth") {
                break;
            }
            let value = if let Some(value) = inline {
                OsString::from(value)
            } else {
                index += 1;
                arguments
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value"))?
            };
            if value.is_empty() {
                return Err(format!("{name} requires a value"));
            }
            if name == "--env-dir" {
                targets.directories.push(PathBuf::from(value));
            } else {
                targets.max_depth = Some(
                    value
                        .to_str()
                        .and_then(|value| value.parse().ok())
                        .filter(|depth| *depth > 0)
                        .ok_or_else(|| {
                            format!(
                                "--max-depth expects a positive number of directory levels, got {}",
                                value.to_string_lossy()
                            )
                        })?,
                );
            }
            index += 1;
        }
        if !targets.directories.is_empty() && targets.max_depth.is_some() {
            return Err(
                "--max-depth limits the search, which --env-dir replaces; use one of them"
                    .to_owned(),
            );
        }
        Ok((targets, index))
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.directories.is_empty() && self.max_depth.is_none()
    }

    pub(crate) const fn names_directories(&self) -> bool {
        !self.directories.is_empty()
    }

    pub(crate) const fn has_max_depth(&self) -> bool {
        self.max_depth.is_some()
    }

    pub(crate) fn max_depth(&self) -> usize {
        self.max_depth.unwrap_or(DEFAULT_MAX_DEPTH)
    }

    pub(crate) fn directories(&self, root: &Path) -> Vec<PathBuf> {
        self.directories
            .iter()
            .map(|directory| root.join(directory))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn parse(arguments: &[&str]) -> Result<(EnvironmentTargets, usize), String> {
        EnvironmentTargets::parse_leading(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn leading_options_stop_at_the_command_word_and_keep_forwarded_arguments() {
        let (targets, consumed) = parse(&[
            "--env-dir",
            "live/prod",
            "--env-dir=live/stg",
            "plan",
            "--env-dir=forwarded",
        ])
        .unwrap();

        assert_eq!(consumed, 3);
        assert_eq!(
            targets.directories(Path::new("/repo")),
            [
                PathBuf::from("/repo/live/prod"),
                PathBuf::from("/repo/live/stg")
            ]
        );
        assert!(!targets.has_max_depth());
    }

    #[test]
    fn terraform_global_options_are_not_terraleph_options() {
        let (targets, consumed) = parse(&["-chdir=live", "plan"]).unwrap();

        assert_eq!(consumed, 0);
        assert!(targets.is_empty());
    }

    #[rstest]
    #[case::separate(&["--max-depth", "6", "tofu"])]
    #[case::inline(&["--max-depth=6"])]
    fn max_depth_accepts_both_forms(#[case] arguments: &[&str]) {
        let (targets, _) = parse(arguments).unwrap();

        assert_eq!(targets.max_depth(), 6);
    }

    #[test]
    fn search_depth_defaults_when_not_given() {
        assert_eq!(EnvironmentTargets::default().max_depth(), DEFAULT_MAX_DEPTH);
    }

    #[rstest]
    #[case::missing_directory(&["--env-dir"])]
    #[case::empty_directory(&["--env-dir="])]
    #[case::zero_depth(&["--max-depth=0"])]
    #[case::non_numeric_depth(&["--max-depth", "deep"])]
    #[case::conflicting(&["--env-dir", "a", "--max-depth", "2"])]
    fn invalid_options_are_usage_errors(#[case] arguments: &[&str]) {
        assert!(parse(arguments).is_err());
    }
}
