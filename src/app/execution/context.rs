use std::{
    borrow::Cow,
    ffi::OsStr,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use crate::app::environments::is_production_token;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Terraform,
    OpenTofu,
}

impl Tool {
    #[must_use]
    pub(crate) const fn executable_name(self) -> &'static str {
        match self {
            Self::Terraform => "terraform",
            Self::OpenTofu => "tofu",
        }
    }

    #[must_use]
    pub(crate) const fn display_name(self) -> &'static str {
        self.executable_name()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutionContextValue {
    Loading,
    Known(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolVersion {
    Loading,
    Known(String),
    Unavailable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct VariableSources {
    automatic_files: Vec<PathBuf>,
    explicit_files: Vec<PathBuf>,
    has_var_argument: bool,
    environment_variables: Vec<String>,
}

impl VariableSources {
    #[must_use]
    pub(crate) const fn new(
        automatic_files: Vec<PathBuf>,
        explicit_files: Vec<PathBuf>,
        has_var_argument: bool,
        environment_variables: Vec<String>,
    ) -> Self {
        Self {
            automatic_files,
            explicit_files,
            has_var_argument,
            environment_variables,
        }
    }

    #[must_use]
    pub(crate) fn automatic_files(&self) -> &[PathBuf] {
        &self.automatic_files
    }

    #[must_use]
    pub(crate) fn explicit_files(&self) -> &[PathBuf] {
        &self.explicit_files
    }

    #[must_use]
    pub(crate) const fn has_var_argument(&self) -> bool {
        self.has_var_argument
    }

    #[must_use]
    pub(crate) fn environment_variables(&self) -> &[String] {
        &self.environment_variables
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionContext {
    cwd: PathBuf,
    launch_root: Option<PathBuf>,
    workspace: ExecutionContextValue,
    display_name: ExecutionContextValue,
    production: Option<bool>,
    tool: Tool,
    tool_version: ToolVersion,
    variable_sources: VariableSources,
}

impl ExecutionContext {
    pub(crate) fn loading(cwd: impl Into<PathBuf>) -> Self {
        Self {
            launch_root: None,
            cwd: cwd.into(),
            workspace: ExecutionContextValue::Loading,
            display_name: ExecutionContextValue::Loading,
            production: None,
            tool: Tool::Terraform,
            tool_version: ToolVersion::Loading,
            variable_sources: VariableSources::default(),
        }
    }

    pub(crate) fn with_launch_root(mut self, launch_root: impl AsRef<Path>) -> Self {
        self.launch_root = Some(launch_root.as_ref().to_owned());
        self
    }

    pub(crate) fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        let workspace = workspace.into();
        self.display_name = ExecutionContextValue::Known(display_name(&self.cwd, &workspace));
        self.production = Some(is_production(&self.cwd, &workspace));
        self.workspace = ExecutionContextValue::Known(workspace);
        self
    }

    pub(crate) const fn with_tool(mut self, tool: Tool) -> Self {
        self.tool = tool;
        self
    }

    pub(crate) fn with_tool_version(mut self, tool: Tool, version: Option<&str>) -> Self {
        self.tool = tool;
        self.tool_version = version.map_or(ToolVersion::Unavailable, |version| {
            ToolVersion::Known(version.to_owned())
        });
        self
    }

    pub(crate) fn with_variable_sources(mut self, variable_sources: VariableSources) -> Self {
        self.variable_sources = variable_sources;
        self
    }

    #[must_use]
    pub(crate) fn cwd_path(&self) -> &Path {
        &self.cwd
    }

    #[must_use]
    pub(crate) fn launch_root_path(&self) -> Option<&Path> {
        self.launch_root.as_deref()
    }

    #[must_use]
    pub(crate) const fn workspace(&self) -> &ExecutionContextValue {
        &self.workspace
    }

    #[must_use]
    pub(crate) const fn display_name(&self) -> &ExecutionContextValue {
        &self.display_name
    }

    #[must_use]
    pub(crate) const fn is_production(&self) -> Option<bool> {
        self.production
    }

    #[must_use]
    pub(crate) const fn tool_name(&self) -> &str {
        self.tool.display_name()
    }

    #[must_use]
    pub(crate) const fn tool(&self) -> Tool {
        self.tool
    }

    #[must_use]
    pub(crate) const fn tool_version(&self) -> &ToolVersion {
        &self.tool_version
    }

    #[must_use]
    pub(crate) const fn variable_sources(&self) -> &VariableSources {
        &self.variable_sources
    }
}

fn display_name(cwd: &Path, workspace: &str) -> String {
    if workspace == "default" {
        directory_display_name(cwd)
    } else {
        workspace.to_owned()
    }
}

// The name doubles as the typed apply confirmation token, so a non-UTF-8 name
// must stay typeable and distinguishable instead of collapsing to U+FFFD.
pub(crate) fn directory_display_name(directory: &Path) -> String {
    escape_non_unicode(directory.file_name().unwrap_or(directory.as_os_str()))
}

fn is_production(cwd: &Path, workspace: &str) -> bool {
    cwd.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .chain(std::iter::once(Cow::Borrowed(workspace)))
        .any(|component| component.split(['-', '_', '/']).any(is_production_token))
}

// Escaping `\` only once a name needs escapes keeps valid names unchanged
// while distinct non-UTF-8 names still get distinct escaped forms.
fn escape_non_unicode(name: &OsStr) -> String {
    if let Some(name) = name.to_str() {
        return name.to_owned();
    }
    let mut escaped = String::new();
    push_escaped_units(&mut escaped, name);
    escaped
}

#[cfg(windows)]
fn push_escaped_units(escaped: &mut String, name: &OsStr) {
    use std::os::windows::ffi::OsStrExt;
    for unit in char::decode_utf16(name.encode_wide()) {
        match unit {
            Ok(character) => push_escaped_char(escaped, character),
            Err(error) => {
                let _ = write!(escaped, "\\u{{{:x}}}", error.unpaired_surrogate());
            }
        }
    }
}

#[cfg(not(windows))]
fn push_escaped_units(escaped: &mut String, name: &OsStr) {
    for chunk in name.as_encoded_bytes().utf8_chunks() {
        chunk
            .valid()
            .chars()
            .for_each(|character| push_escaped_char(escaped, character));
        for byte in chunk.invalid() {
            let _ = write!(escaped, "\\x{byte:02x}");
        }
    }
}

fn push_escaped_char(escaped: &mut String, character: char) {
    if character == '\\' {
        escaped.push_str("\\\\");
    } else {
        escaped.push(character);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_detection_uses_complete_tokens() {
        struct Case {
            name: &'static str,
            cwd: &'static str,
            workspace: &'static str,
            expected: bool,
        }

        for case in [
            Case {
                name: "prod_directory",
                cwd: "/repo/prod",
                workspace: "default",
                expected: true,
            },
            Case {
                name: "production_prefix",
                cwd: "/repo/productionapp",
                workspace: "default",
                expected: false,
            },
            Case {
                name: "production_and_prod_tokens",
                cwd: "/repo/production/prod",
                workspace: "default",
                expected: true,
            },
            Case {
                name: "workspace_token",
                cwd: "/repo/staging",
                workspace: "prd",
                expected: true,
            },
            Case {
                name: "product_prefix",
                cwd: "/repo/product",
                workspace: "default",
                expected: false,
            },
            Case {
                name: "case_insensitive_production_token",
                cwd: "/repo/Prod",
                workspace: "default",
                expected: true,
            },
            Case {
                name: "production_sort_suffix_does_not_expand_badge",
                cwd: "/repo/prod2",
                workspace: "default",
                expected: false,
            },
            Case {
                name: "live_is_not_a_production_token",
                cwd: "/repo/live",
                workspace: "default",
                expected: false,
            },
        ] {
            assert_eq!(
                is_production(Path::new(case.cwd), case.workspace),
                case.expected,
                "case: {}",
                case.name
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn production_detection_reads_tokens_in_non_utf8_components() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let cwd = PathBuf::from(OsString::from_vec(b"/repo/prod-\xff".to_vec()));

        let context = ExecutionContext::loading(cwd).with_workspace("default");

        assert_eq!(context.is_production(), Some(true));
    }

    #[test]
    fn default_workspace_uses_directory_name_as_display_name() {
        let context = ExecutionContext::loading("/repo/infra/prod").with_workspace("default");

        assert_eq!(
            context.display_name(),
            &ExecutionContextValue::Known("prod".to_owned())
        );
        assert_eq!(context.is_production(), Some(true));
    }

    #[test]
    fn non_default_workspace_is_the_display_name() {
        let context = ExecutionContext::loading("/repo/infra").with_workspace("production");

        assert_eq!(
            context.display_name(),
            &ExecutionContextValue::Known("production".to_owned())
        );
        assert_eq!(context.is_production(), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn display_name_escapes_only_non_utf8_default_workspace_directories() {
        struct Case {
            name: &'static str,
            cwd: &'static [u8],
            workspace: &'static str,
            expected: &'static str,
        }

        for case in [
            Case {
                name: "non_ascii_utf8_directory",
                cwd: "/repo/インフラ".as_bytes(),
                workspace: "default",
                expected: "インフラ",
            },
            Case {
                name: "utf8_directory_with_backslash",
                cwd: br"/repo/infra\dev",
                workspace: "default",
                expected: r"infra\dev",
            },
            Case {
                name: "invalid_byte",
                cwd: b"/repo/infra-\xff",
                workspace: "default",
                expected: r"infra-\xff",
            },
            Case {
                name: "truncated_sequence_before_valid_chunk",
                cwd: b"/repo/\xe3\x81-infra",
                workspace: "default",
                expected: r"\xe3\x81-infra",
            },
            Case {
                name: "backslash_beside_invalid_byte",
                cwd: b"/repo/infra\\\xff",
                workspace: "default",
                expected: r"infra\\\xff",
            },
            Case {
                name: "non_default_workspace_in_invalid_directory",
                cwd: b"/repo/infra-\xff",
                workspace: "staging",
                expected: "staging",
            },
        ] {
            let context =
                ExecutionContext::loading(non_utf8_path(case.cwd)).with_workspace(case.workspace);

            assert_eq!(
                context.display_name(),
                &ExecutionContextValue::Known(case.expected.to_owned()),
                "case: {}",
                case.name
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn distinct_non_utf8_directories_get_distinct_display_names() {
        for (left, right) in [
            (
                b"/repo/infra-\xff".as_slice(),
                b"/repo/infra-\xfe".as_slice(),
            ),
            (
                b"/repo/a\xff\\xfe".as_slice(),
                b"/repo/a\\xff\xfe".as_slice(),
            ),
        ] {
            let left_name =
                ExecutionContext::loading(non_utf8_path(left)).with_workspace("default");
            let right_name =
                ExecutionContext::loading(non_utf8_path(right)).with_workspace("default");

            assert_ne!(left_name.display_name(), right_name.display_name());
        }
    }

    #[cfg(unix)]
    fn non_utf8_path(bytes: &[u8]) -> PathBuf {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }

    #[cfg(windows)]
    #[test]
    fn windows_display_name_escapes_unpaired_surrogates_and_keeps_valid_pairs() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        struct Case {
            name: &'static str,
            file_name: &'static [u16],
            expected: &'static str,
        }

        for case in [
            Case {
                name: "unpaired_high_surrogate",
                file_name: &[0x69, 0x6e, 0x66, 0x72, 0x61, 0x2d, 0xd800],
                expected: r"infra-\u{d800}",
            },
            Case {
                name: "valid_pair_beside_unpaired_low_surrogate",
                file_name: &[0x61, 0xd83d, 0xde00, 0xdc00],
                expected: "a\u{1f600}\\u{dc00}",
            },
        ] {
            let cwd = PathBuf::from(r"C:\repo").join(OsString::from_wide(case.file_name));

            let context = ExecutionContext::loading(cwd).with_workspace("default");

            assert_eq!(
                context.display_name(),
                &ExecutionContextValue::Known(case.expected.to_owned()),
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn opentofu_context_keeps_the_selected_tool_for_the_header() {
        let loading = ExecutionContext::loading("/repo/infra").with_tool(Tool::OpenTofu);
        assert_eq!(loading.tool_name(), "tofu");
        assert_eq!(loading.tool_version(), &ToolVersion::Loading);

        let context = loading.with_tool_version(Tool::OpenTofu, Some("1.10.0"));

        assert_eq!(context.tool_name(), "tofu");
        assert_eq!(
            context.tool_version(),
            &ToolVersion::Known("1.10.0".to_owned())
        );
    }

    #[test]
    fn missing_tool_version_is_unavailable_and_keeps_the_selected_tool() {
        let context =
            ExecutionContext::loading("/repo/infra").with_tool_version(Tool::OpenTofu, None);

        assert_eq!(context.tool_name(), "tofu");
        assert_eq!(context.tool_version(), &ToolVersion::Unavailable);
    }
}
