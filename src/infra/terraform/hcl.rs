use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use hcl::Structure;

use crate::app::attribution::{
    ResourceAddress, ResourceSourceLocation, SourceFileAnalysis, SourceIssue, SourceIssueKind,
    SourceRange, SourceSide,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HclSourceFile {
    path: PathBuf,
    source: String,
    side: SourceSide,
}

impl HclSourceFile {
    #[must_use]
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        source: impl Into<String>,
        side: SourceSide,
    ) -> Self {
        Self {
            path: path.into(),
            source: source.into(),
            side,
        }
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub(crate) fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct HclParseResult {
    files: Vec<SourceFileAnalysis>,
}

impl HclParseResult {
    const fn new(files: Vec<SourceFileAnalysis>) -> Self {
        Self { files }
    }

    #[must_use]
    pub(crate) fn into_files(self) -> Vec<SourceFileAnalysis> {
        self.files
    }
}

#[must_use]
pub(crate) fn parse_source(input: HclSourceFile) -> SourceFileAnalysis {
    if !is_native_hcl_path(&input.path) {
        return SourceFileAnalysis::new(
            input.path,
            input.side,
            Vec::new(),
            vec![SourceIssue::new(
                SourceIssueKind::UnsupportedInput,
                "only native .tf configuration is supported",
            )],
        );
    }

    let (parsed_addresses, mut issues) = parse_hcl(&input.source);
    let (mut resources, scan_issues) = scan_resources(&input);
    issues.extend(scan_issues);
    if issues
        .iter()
        .any(|issue| issue.kind() == SourceIssueKind::SyntaxError)
    {
        resources.clear();
    } else {
        for (resource, address) in resources.iter_mut().zip(parsed_addresses) {
            resource.set_address(address);
        }
    }

    SourceFileAnalysis::new(input.path, input.side, resources, issues)
}

#[must_use]
pub(crate) fn parse_files<I>(inputs: I) -> HclParseResult
where
    I: IntoIterator<Item = HclSourceFile>,
{
    let mut result = HclParseResult::new(inputs.into_iter().map(parse_source).collect());
    mark_duplicate_resources(&mut result);
    result
}

fn parse_hcl(source: &str) -> (Vec<ResourceAddress>, Vec<SourceIssue>) {
    match hcl::parse(source) {
        Ok(body) => (parsed_resource_addresses(body), Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![SourceIssue::new(
                SourceIssueKind::SyntaxError,
                format!("HCL parse error: {error}"),
            )],
        ),
    }
}

fn parsed_resource_addresses(body: hcl::Body) -> Vec<ResourceAddress> {
    body.into_inner()
        .into_iter()
        .filter_map(|structure| match structure {
            Structure::Block(block)
                if block.identifier() == "resource" && block.labels().len() == 2 =>
            {
                Some(ResourceAddress::new(
                    block.labels()[0].as_str(),
                    block.labels()[1].as_str(),
                ))
            }
            _ => None,
        })
        .collect()
}

fn scan_resources(input: &HclSourceFile) -> (Vec<ResourceSourceLocation>, Vec<SourceIssue>) {
    let scanned = scan(&input.source);
    let mut resources = Vec::new();
    let mut issues = scanned
        .errors
        .into_iter()
        .map(|message| SourceIssue::new(SourceIssueKind::SyntaxError, message))
        .collect::<Vec<_>>();
    let mut braces = Vec::new();

    for (index, token) in scanned.tokens.iter().enumerate() {
        match &token.kind {
            TokenKind::OpenBrace => {
                let resource = if braces.is_empty() {
                    resource_header(&scanned.tokens, index)
                } else {
                    None
                };
                braces.push(Brace {
                    line: token.line,
                    resource,
                });
            }
            TokenKind::CloseBrace => match braces.pop() {
                Some(brace) => {
                    if let Some((address, start_line)) = brace.resource {
                        resources.push(ResourceSourceLocation::new(
                            address,
                            input.path.clone(),
                            input.side,
                            SourceRange::new(start_line, token.line),
                        ));
                    }
                }
                None => issues.push(SourceIssue::new(
                    SourceIssueKind::SyntaxError,
                    format!("unexpected closing brace on line {}", token.line),
                )),
            },
            TokenKind::Identifier(identifier)
                if braces.is_empty()
                    && identifier == "resource"
                    && resource_keyword_is_malformed(&scanned.tokens, index) =>
            {
                issues.push(SourceIssue::new(
                    SourceIssueKind::SyntaxError,
                    format!("malformed resource block near line {}", token.line),
                ));
            }
            _ => {}
        }
    }

    for brace in braces {
        issues.push(SourceIssue::new(
            SourceIssueKind::SyntaxError,
            format!("unclosed block starting on line {}", brace.line),
        ));
    }

    resources.sort_by_key(|resource| (resource.start_line(), resource.end_line()));
    (resources, issues)
}

fn resource_header(tokens: &[Token], open_index: usize) -> Option<(ResourceAddress, usize)> {
    let [keyword, resource_type, name] = tokens.get(open_index.checked_sub(3)?..open_index)? else {
        return None;
    };
    let start_line = keyword.line;

    match (&keyword.kind, &resource_type.kind, &name.kind) {
        (
            TokenKind::Identifier(keyword),
            TokenKind::String(resource_type),
            TokenKind::String(name),
        ) if keyword == "resource" => Some((
            ResourceAddress::new(resource_type.clone(), name.clone()),
            start_line,
        )),
        _ => None,
    }
}

fn resource_keyword_is_malformed(tokens: &[Token], index: usize) -> bool {
    let Some(first_label) = tokens.get(index + 1) else {
        return true;
    };
    if !matches!(first_label.kind, TokenKind::String(_)) {
        return !matches!(first_label.kind, TokenKind::Other(b'='));
    }

    let Some(second_label) = tokens.get(index + 2) else {
        return true;
    };
    if !matches!(second_label.kind, TokenKind::String(_)) {
        return true;
    }

    !matches!(
        tokens.get(index + 3).map(|token| &token.kind),
        Some(TokenKind::OpenBrace)
    )
}

fn mark_duplicate_resources(result: &mut HclParseResult) {
    let mut occurrences = HashMap::<(SourceSide, ResourceAddress), Vec<(usize, usize)>>::new();

    for (file_index, file) in result.files.iter().enumerate() {
        for (resource_index, resource) in file.resources().iter().enumerate() {
            occurrences
                .entry((resource.side(), resource.address().clone()))
                .or_default()
                .push((file_index, resource_index));
        }
    }

    for ((side, address), locations) in occurrences {
        if locations.len() < 2 {
            continue;
        }

        let files = locations
            .iter()
            .map(|(file_index, _)| result.files[*file_index].path().display().to_string())
            .collect::<Vec<_>>();
        let message = format!(
            "resource {}.{} is defined more than once for {:?}: {}",
            address.resource_type(),
            address.name(),
            side,
            files.join(", ")
        );
        let mut marked_files = Vec::new();
        for (file_index, _) in locations {
            if !marked_files.contains(&file_index) {
                result.files[file_index].issues_mut().push(SourceIssue::new(
                    SourceIssueKind::DuplicateResource,
                    message.clone(),
                ));
                marked_files.push(file_index);
            }
        }
    }
}

fn is_native_hcl_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "tf")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Identifier(String),
    String(String),
    OpenBrace,
    CloseBrace,
    Other(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanResult {
    tokens: Vec<Token>,
    errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Brace {
    line: usize,
    resource: Option<(ResourceAddress, usize)>,
}

fn scan(source: &str) -> ScanResult {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut index = 0;
    let mut line_starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(index + 1);
        }
    }

    while index < source.len() {
        let bytes = source.as_bytes();
        match bytes[index] {
            b' ' | b'\t' | b'\r' | b'\n' => index += 1,
            b'#' => index = skip_line(source, index + 1),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line(source, index + 2);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                if let Some(end) = source[index + 2..].find("*/") {
                    index += end + 4;
                } else {
                    errors.push(format!(
                        "unterminated block comment on line {}",
                        line_number(&line_starts, index)
                    ));
                    break;
                }
            }
            b'"' => match scan_string(source, index, &line_starts) {
                Ok((end, value)) => {
                    tokens.push(Token {
                        kind: TokenKind::String(value),
                        line: line_number(&line_starts, index),
                    });
                    index = end;
                }
                Err((end, message)) => {
                    errors.push(message);
                    index = end;
                }
            },
            b'<' if bytes.get(index + 1) == Some(&b'<') => {
                match skip_heredoc(source, index, &line_starts) {
                    Ok(end) => index = end,
                    Err(message) => {
                        errors.push(message);
                        break;
                    }
                }
            }
            b'{' => {
                tokens.push(Token {
                    kind: TokenKind::OpenBrace,
                    line: line_number(&line_starts, index),
                });
                index += 1;
            }
            b'}' => {
                tokens.push(Token {
                    kind: TokenKind::CloseBrace,
                    line: line_number(&line_starts, index),
                });
                index += 1;
            }
            byte if is_identifier_start(byte) => {
                let start = index;
                index += 1;
                while index < source.len() && is_identifier_continue(source.as_bytes()[index]) {
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Identifier(source[start..index].to_owned()),
                    line: line_number(&line_starts, start),
                });
            }
            _ => {
                tokens.push(Token {
                    kind: TokenKind::Other(bytes[index]),
                    line: line_number(&line_starts, index),
                });
                index += source[index..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }

    ScanResult { tokens, errors }
}

fn scan_string(
    source: &str,
    start: usize,
    line_starts: &[usize],
) -> Result<(usize, String), (usize, String)> {
    let mut index = start + 1;
    while index < source.len() {
        match source.as_bytes()[index] {
            b'\\' => {
                index += 1;
                if index < source.len() {
                    index += 1;
                }
            }
            b'$' if source.as_bytes().get(index + 1) == Some(&b'$')
                && source.as_bytes().get(index + 2) == Some(&b'{') =>
            {
                index += 3;
            }
            b'%' if source.as_bytes().get(index + 1) == Some(&b'%')
                && source.as_bytes().get(index + 2) == Some(&b'{') =>
            {
                index += 3;
            }
            b'$' | b'%' if source.as_bytes().get(index + 1) == Some(&b'{') => {
                match skip_template_expression(source, index + 2, line_starts) {
                    Ok(end) => index = end,
                    Err(message) => return Err((source.len(), message)),
                }
            }
            b'"' => {
                return Ok((index + 1, source[start + 1..index].to_owned()));
            }
            b'\n' => {
                return Err((
                    index + 1,
                    format!(
                        "unterminated string on line {}",
                        line_number(line_starts, start)
                    ),
                ));
            }
            _ => index += 1,
        }
    }

    Err((
        source.len(),
        format!(
            "unterminated string on line {}",
            line_number(line_starts, start)
        ),
    ))
}

fn skip_template_expression(
    source: &str,
    start: usize,
    line_starts: &[usize],
) -> Result<usize, String> {
    let mut index = start;
    let mut depth = 1;
    while index < source.len() {
        let bytes = source.as_bytes();
        match bytes[index] {
            b'"' => match scan_string(source, index, line_starts) {
                Ok((end, _)) => index = end,
                Err((_, message)) => return Err(message),
            },
            b'#' => index = skip_line(source, index + 1),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line(source, index + 2);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                if let Some(end) = source[index + 2..].find("*/") {
                    index += end + 4;
                } else {
                    return Err(format!(
                        "unterminated block comment on line {}",
                        line_number(line_starts, index)
                    ));
                }
            }
            b'<' if bytes.get(index + 1) == Some(&b'<') => {
                index = skip_heredoc(source, index, line_starts)?;
            }
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => index += source[index..].chars().next().map_or(1, char::len_utf8),
        }
    }

    Err(format!(
        "unterminated template expression on line {}",
        line_number(line_starts, start.saturating_sub(2))
    ))
}

fn skip_heredoc(source: &str, start: usize, line_starts: &[usize]) -> Result<usize, String> {
    let mut index = start + 2;
    let indented = source.as_bytes().get(index) == Some(&b'-');
    if indented {
        index += 1;
    }
    while matches!(source.as_bytes().get(index), Some(b' ' | b'\t')) {
        index += 1;
    }

    let delimiter_start = index;
    while index < source.len() && source.as_bytes()[index] != b'\n' {
        index += 1;
    }
    let delimiter = source[delimiter_start..index].trim_end_matches('\r').trim();
    if delimiter.is_empty() {
        return Err(format!(
            "missing heredoc delimiter on line {}",
            line_number(line_starts, start)
        ));
    }

    if index == source.len() {
        return Err(format!(
            "unterminated heredoc on line {}",
            line_number(line_starts, start)
        ));
    }
    index += 1;

    while index <= source.len() {
        let line_end = source[index..]
            .find('\n')
            .map_or(source.len(), |offset| index + offset);
        let line = source[index..line_end].trim_end_matches('\r');
        let candidate = if indented {
            line.trim_start_matches([' ', '\t'])
        } else {
            line
        };
        if candidate.trim_end_matches([' ', '\t']) == delimiter {
            return Ok(line_end.saturating_add(usize::from(line_end < source.len())));
        }
        if line_end == source.len() {
            break;
        }
        index = line_end + 1;
    }

    Err(format!(
        "unterminated heredoc on line {}",
        line_number(line_starts, start)
    ))
}

const fn skip_line(source: &str, mut index: usize) -> usize {
    while index < source.len() && source.as_bytes()[index] != b'\n' {
        index += 1;
    }
    index
}

fn line_number(line_starts: &[usize], offset: usize) -> usize {
    match line_starts.binary_search(&offset) {
        Ok(line) => line + 1,
        Err(line) => line,
    }
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit() || byte == b'-'
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    impl HclParseResult {
        fn is_complete(&self) -> bool {
            self.files.iter().all(SourceFileAnalysis::is_complete)
        }

        fn resources(&self) -> impl Iterator<Item = &ResourceSourceLocation> {
            self.files.iter().flat_map(|file| file.resources().iter())
        }

        fn file(&self, path: &Path, side: SourceSide) -> Option<&SourceFileAnalysis> {
            self.files
                .iter()
                .find(|file| file.path() == path && file.side() == side)
        }
    }

    impl SourceFileAnalysis {
        fn has_issue(&self, kind: SourceIssueKind) -> bool {
            self.issues().iter().any(|issue| issue.kind() == kind)
        }
    }

    fn after(source: &str) -> HclSourceFile {
        HclSourceFile::new("main.tf", source, SourceSide::After)
    }

    #[test]
    fn locates_multiple_resources_and_nested_braces() {
        let result = parse_files([after(
            r#"resource "aws_instance" "api" {
  tags = { Name = "api" }
  provisioner "local-exec" {
    command = "echo {not-a-block}"
  }
}

resource "aws_s3_bucket" "logs" {
  bucket = "logs"
}
"#,
        )]);

        let resources = result.resources().collect::<Vec<_>>();

        assert_eq!(resources.len(), 2);
        assert_eq!(
            resources[0].address(),
            &ResourceAddress::new("aws_instance", "api")
        );
        assert_eq!(resources[0].range(), SourceRange::new(1, 6));
        assert_eq!(
            resources[1].address(),
            &ResourceAddress::new("aws_s3_bucket", "logs")
        );
        assert_eq!(resources[1].range(), SourceRange::new(8, 10));
        assert!(result.is_complete());
    }

    #[test]
    fn ignores_braces_in_comments_and_heredocs() {
        let result = parse_files([after(
            r#"# } ignored
resource "test_resource" "example" {
  command = <<-EOT
    { ignored }
  EOT
  // { ignored }
  value = "}"
}
"#,
        )]);

        let resources = result.resources().collect::<Vec<_>>();

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].range(), SourceRange::new(2, 8));
        assert!(result.is_complete());
    }

    #[test]
    fn ignores_template_interpolation_strings_and_braces() {
        let result = parse_files([after(
            r#"resource "terraform_data" "main" {
  input = "${format("%s}", "x")}"
}
"#,
        )]);

        let resource = result.resources().next().expect("resource source block");
        assert_eq!(
            resource.address(),
            &ResourceAddress::new("terraform_data", "main")
        );
        assert_eq!(resource.range(), SourceRange::new(1, 3));
        assert!(result.is_complete());
    }

    #[test]
    fn ignores_escaped_template_openers() {
        let result = parse_files([after(
            r#"resource "terraform_data" "escaped" {
  input = "$${literal"
}

resource "terraform_data" "directive" {
  input = "%%{literal"
}
"#,
        )]);

        let resources = result.resources().collect::<Vec<_>>();
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].range(), SourceRange::new(1, 3));
        assert_eq!(resources[1].range(), SourceRange::new(5, 7));
        assert!(result.is_complete());
    }

    #[test]
    fn keeps_valid_files_when_another_file_has_syntax_error() {
        let result = parse_files([
            HclSourceFile::new(
                "broken.tf",
                "resource \"bad\" \"resource\" {}\n\nlocals {\n",
                SourceSide::After,
            ),
            after("resource \"aws_vpc\" \"main\" {}\n"),
        ]);

        assert!(!result.is_complete());
        assert_eq!(
            result
                .file(Path::new("main.tf"), SourceSide::After)
                .expect("valid file")
                .resources()
                .len(),
            1
        );
        assert!(
            result
                .file(Path::new("broken.tf"), SourceSide::After)
                .expect("broken file")
                .has_issue(SourceIssueKind::SyntaxError)
        );
        assert_eq!(
            result
                .file(Path::new("broken.tf"), SourceSide::After)
                .expect("broken file")
                .resources()
                .len(),
            0
        );
    }

    #[test]
    fn distinguishes_same_name_with_different_types() {
        let result = parse_files([
            after("resource \"aws_vpc\" \"shared\" {}\n"),
            HclSourceFile::new(
                "other.tf",
                "resource \"aws_subnet\" \"shared\" {}\n",
                SourceSide::After,
            ),
        ]);

        assert!(result.is_complete());
        assert_eq!(result.resources().count(), 2);
    }

    #[test]
    fn marks_duplicate_definitions_as_incomplete() {
        let result = parse_files([
            after("resource \"aws_vpc\" \"main\" {}\n"),
            HclSourceFile::new(
                "other.tf",
                "resource \"aws_vpc\" \"main\" {}\n",
                SourceSide::After,
            ),
        ]);

        assert!(!result.is_complete());
        assert!(
            result
                .files
                .iter()
                .all(|file| file.has_issue(SourceIssueKind::DuplicateResource))
        );
    }

    #[test]
    fn marks_json_configuration_as_unsupported() {
        let result = parse_files([HclSourceFile::new("main.tf.json", "{}", SourceSide::After)]);

        assert!(!result.is_complete());
        assert!(result.files[0].has_issue(SourceIssueKind::UnsupportedInput));
        assert_eq!(result.files[0].resources().len(), 0);
    }

    #[test]
    fn parses_deleted_before_source() {
        let result = parse_files([HclSourceFile::new(
            "removed.tf",
            "resource \"aws_instance\" \"old\" {}\n",
            SourceSide::Before,
        )]);

        let resource = result.resources().next().expect("deleted source block");
        assert_eq!(resource.side(), SourceSide::Before);
        assert_eq!(
            resource.address(),
            &ResourceAddress::new("aws_instance", "old")
        );
    }

    #[test]
    fn decodes_escaped_resource_labels() {
        let result = parse_files([after(
            r#"resource "terraform_\u0064ata" "\u006dain" {
  input = "ok"
}
"#,
        )]);

        let resource = result.resources().next().expect("resource source block");
        assert_eq!(
            resource.address(),
            &ResourceAddress::new("terraform_data", "main")
        );
        assert!(result.is_complete());
    }

    #[rstest]
    #[case::incomplete_block("resource \"aws_vpc\" \"main\" {\n  cidr_block =\n}\n")]
    #[case::resource_expression("value = resource \"aws_vpc\" \"main\" {}\n")]
    #[case::malformed_resource_header("resource \"aws_instance\" {\n}\n")]
    fn rejects_malformed_hcl_without_resources(#[case] source: &str) {
        let result = parse_files([after(source)]);

        assert!(!result.is_complete());
        assert!(result.files[0].has_issue(SourceIssueKind::SyntaxError));
        assert_eq!(result.files[0].resources().len(), 0);
    }
}
