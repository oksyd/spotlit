#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AppOptions {
    pub(crate) background: bool,
}

impl AppOptions {
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut options = Self::default();
        for arg in args {
            if arg == "--background" {
                options.background = true;
            }
        }
        options
    }
}

#[cfg(test)]
mod tests {
    use super::AppOptions;

    #[test]
    fn parses_background_startup_flag() {
        let options = AppOptions::parse(["--background".to_string(), "--ignored".to_string()]);

        assert!(options.background);
    }
}
