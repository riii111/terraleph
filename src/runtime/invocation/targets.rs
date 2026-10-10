use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnvironmentTargets {
    directories: Vec<PathBuf>,
}

impl EnvironmentTargets {
    /// # Errors
    ///
    /// Returns a usage message when an option lacks a value.
    pub fn parse_leading(arguments: &[OsString]) -> Result<(Self, usize), String> {
        let mut targets = Self::default();
        let mut index = 0;
        while let Some(argument) = arguments.get(index).and_then(|argument| argument.to_str()) {
            let (name, inline) = argument
                .split_once('=')
                .map_or((argument, None), |(name, value)| (name, Some(value)));
            if name != "--env-dir" {
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
            targets.directories.push(PathBuf::from(value));
            index += 1;
        }
        Ok((targets, index))
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.directories.is_empty()
    }

    pub(crate) const fn names_directories(&self) -> bool {
        !self.directories.is_empty()
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
    }

    #[test]
    fn terraform_global_options_are_not_terraleph_options() {
        let (targets, consumed) = parse(&["-chdir=live", "plan"]).unwrap();

        assert_eq!(consumed, 0);
        assert!(targets.is_empty());
    }

    #[rstest]
    #[case::missing_directory(&["--env-dir"])]
    #[case::empty_directory(&["--env-dir="])]
    fn invalid_options_are_usage_errors(#[case] arguments: &[&str]) {
        assert!(parse(arguments).is_err());
    }
}
