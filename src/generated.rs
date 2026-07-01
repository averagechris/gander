use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GeneratedPreset {
    Lockfiles,
    ApiClients,
    VendoredAssets,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeneratedPolicy {
    pub presets: Vec<GeneratedPreset>,
    pub globs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GeneratedMatcher {
    set: GlobSet,
}

impl GeneratedMatcher {
    pub fn new(policy: &GeneratedPolicy) -> Result<Self, globset::Error> {
        let mut builder = GlobSetBuilder::new();
        for pattern in policy.patterns() {
            builder.add(Glob::new(&pattern)?);
        }
        Ok(Self {
            set: builder.build()?,
        })
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.set.is_match(path)
    }
}

impl GeneratedPolicy {
    fn patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();
        for preset in &self.presets {
            patterns.extend(
                preset
                    .patterns()
                    .iter()
                    .map(|pattern| (*pattern).to_owned()),
            );
        }
        patterns.extend(self.globs.clone());
        patterns
    }
}

impl GeneratedPreset {
    fn patterns(&self) -> &'static [&'static str] {
        match self {
            Self::Lockfiles => &[
                "Cargo.lock",
                "package-lock.json",
                "pnpm-lock.yaml",
                "yarn.lock",
                "Gemfile.lock",
                "go.sum",
                "poetry.lock",
            ],
            Self::ApiClients => &[
                "**/generated/**",
                "**/gen/**",
                "**/*generated*",
                "**/openapi/**",
                "**/graphql/generated/**",
                "**/__generated__/**",
            ],
            Self::VendoredAssets => &[
                "**/*.min.js",
                "**/*.min.css",
                "vendor/**",
                "third_party/**",
                "dist/**",
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_preset_matches_common_lockfiles() {
        let matcher = GeneratedMatcher::new(&GeneratedPolicy {
            presets: vec![GeneratedPreset::Lockfiles],
            globs: Vec::new(),
        })
        .unwrap();

        assert!(matcher.is_match("Cargo.lock"));
        assert!(matcher.is_match("pnpm-lock.yaml"));
        assert!(!matcher.is_match("Cargo.toml"));
    }

    #[test]
    fn api_client_preset_matches_nested_generated_paths() {
        let matcher = GeneratedMatcher::new(&GeneratedPolicy {
            presets: vec![GeneratedPreset::ApiClients],
            globs: Vec::new(),
        })
        .unwrap();

        assert!(matcher.is_match("src/graphql/generated/types.ts"));
        assert!(matcher.is_match("client/__generated__/schema.ts"));
        assert!(!matcher.is_match("src/app.rs"));
    }

    #[test]
    fn custom_globs_are_combined_with_presets() {
        let matcher = GeneratedMatcher::new(&GeneratedPolicy {
            presets: vec![GeneratedPreset::Lockfiles],
            globs: vec!["schemas/*.json".to_owned()],
        })
        .unwrap();

        assert!(matcher.is_match("Cargo.lock"));
        assert!(matcher.is_match("schemas/openapi.json"));
    }

    #[test]
    fn invalid_custom_glob_errors() {
        let result = GeneratedMatcher::new(&GeneratedPolicy {
            presets: Vec::new(),
            globs: vec!["[".to_owned()],
        });

        assert!(result.is_err());
    }
}
