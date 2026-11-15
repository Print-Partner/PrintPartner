use anyhow::{Context, Result, bail};
use std::path::PathBuf;

#[derive(Debug, Default)]
pub(crate) struct Arguments {
    pub(crate) data: Option<PathBuf>,
    pub(crate) web: Option<PathBuf>,
    pub(crate) node: Option<PathBuf>,
    pub(crate) commit: Option<String>,
    pub(crate) credential_file: Option<PathBuf>,
    pub(crate) status_file: Option<PathBuf>,
}

pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Arguments> {
    let mut parsed = Arguments::default();
    while let Some(arg) = args.next() {
        let value = args.next().context("Argument value missing")?;
        match arg.as_str() {
            "--data" => parsed.data = Some(PathBuf::from(value)),
            "--web" => parsed.web = Some(PathBuf::from(value)),
            "--node" => parsed.node = Some(PathBuf::from(value)),
            "--commit" => parsed.commit = Some(value),
            "--test-credential-file" => parsed.credential_file = Some(PathBuf::from(value)),
            "--status-file" => parsed.status_file = Some(PathBuf::from(value)),
            _ => bail!("Unknown argument"),
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Arguments> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_all_arguments_without_opening_paths() {
        let parsed = parse_args(&[
            "--data",
            "test-data",
            "--web",
            "test-web",
            "--node",
            "test-node",
            "--commit",
            "test-commit",
            "--test-credential-file",
            "test-receipt",
            "--status-file",
            "test-status",
        ])
        .expect("valid arguments");
        assert_eq!(parsed.data, Some(PathBuf::from("test-data")));
        assert_eq!(parsed.web, Some(PathBuf::from("test-web")));
        assert_eq!(parsed.node, Some(PathBuf::from("test-node")));
        assert_eq!(parsed.commit.as_deref(), Some("test-commit"));
        assert_eq!(parsed.credential_file, Some(PathBuf::from("test-receipt")));
        assert_eq!(parsed.status_file, Some(PathBuf::from("test-status")));
    }

    #[test]
    fn absent_values_remain_for_startup_validation() {
        let parsed = parse_args(&[]).expect("empty input");
        assert!(parsed.data.is_none());
        assert!(parsed.web.is_none());
        assert!(parsed.node.is_none());
        assert!(parsed.commit.is_none());
        assert!(parsed.credential_file.is_none());
        assert!(parsed.status_file.is_none());
    }

    #[test]
    fn duplicate_options_keep_the_last_value() {
        let parsed =
            parse_args(&["--commit", "first", "--commit", "last"]).expect("repeated option");
        assert_eq!(parsed.commit.as_deref(), Some("last"));
    }

    #[test]
    fn missing_value_is_rejected() {
        let error = parse_args(&["--data"]).expect_err("missing value");
        assert_eq!(error.to_string(), "Argument value missing");
    }

    #[test]
    fn unknown_option_is_rejected() {
        let error = parse_args(&["--unknown", "value"]).expect_err("unknown option");
        assert_eq!(error.to_string(), "Unknown argument");
    }
}
