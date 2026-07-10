use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, bail, eyre};
use serde::Serialize;

const MAX_SKILL_BYTES: usize = 16 * 1024;

const BUNDLED_SKILLS: &[&str] = &[
    include_str!("../skills/gander-review/SKILL.md"),
    include_str!("../skills/gander-address-review/SKILL.md"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledSkill {
    pub metadata: SkillMetadata,
    pub markdown: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledSkill {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallPlan {
    pub selected: Vec<SkillMetadata>,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frontmatter {
    name: String,
    description: String,
}

pub fn list() -> Result<Vec<SkillMetadata>> {
    bundled_skills().map(|skills| skills.into_iter().map(|s| s.metadata).collect())
}

pub fn show(name: &str) -> Result<&'static str> {
    Ok(find_skill(name)?.markdown)
}

pub fn find_skill(name: &str) -> Result<BundledSkill> {
    bundled_skills()?
        .into_iter()
        .find(|skill| skill.metadata.name == name)
        .ok_or_else(|| eyre!("unknown skill {name}"))
}

pub fn install(base_dir: &Path, names: &[String], force: bool) -> Result<Vec<InstalledSkill>> {
    let skills = select_skills(names)?;
    preflight_install(base_dir, &skills, force)?;

    let mut installed = Vec::with_capacity(skills.len());
    for skill in skills {
        let path = skill_path(base_dir, &skill.metadata.name);
        let parent = path
            .parent()
            .ok_or_else(|| eyre!("skill path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)?;
        fs::write(&path, skill.markdown)?;
        installed.push(InstalledSkill {
            name: skill.metadata.name,
            path,
        });
    }
    Ok(installed)
}

pub fn preflight_install(
    base_dir: &Path,
    skills: &[BundledSkill],
    force: bool,
) -> Result<InstallPlan> {
    let mut names = HashSet::new();
    let mut selected = Vec::with_capacity(skills.len());
    let mut paths = Vec::with_capacity(skills.len());
    for skill in skills {
        if !names.insert(skill.metadata.name.as_str()) {
            bail!("duplicate requested skill {}", skill.metadata.name);
        }
        let path = skill_path(base_dir, &skill.metadata.name);
        if path.exists() && !force {
            bail!(
                "refusing to overwrite {}; pass force to replace",
                path.display()
            );
        }
        selected.push(skill.metadata.clone());
        paths.push(path);
    }
    Ok(InstallPlan { selected, paths })
}

pub fn select_skills(names: &[String]) -> Result<Vec<BundledSkill>> {
    let all = bundled_skills()?;
    if names.is_empty() {
        return Ok(all);
    }
    let mut seen = HashSet::new();
    let mut selected = Vec::with_capacity(names.len());
    for name in names {
        if !seen.insert(name.as_str()) {
            bail!("duplicate requested skill {name}");
        }
        selected.push(
            all.iter()
                .find(|s| s.metadata.name == *name)
                .cloned()
                .ok_or_else(|| eyre!("unknown skill {name}"))?,
        );
    }
    Ok(selected)
}

fn bundled_skills() -> Result<Vec<BundledSkill>> {
    BUNDLED_SKILLS
        .iter()
        .map(|markdown| {
            if markdown.len() > MAX_SKILL_BYTES {
                bail!("bundled skill exceeds {MAX_SKILL_BYTES} bytes");
            }
            let fm = parse_frontmatter(markdown)?;
            Ok(BundledSkill {
                metadata: SkillMetadata {
                    name: fm.name,
                    description: fm.description,
                },
                markdown,
            })
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|skills| {
            let mut names = HashSet::new();
            for skill in &skills {
                if !names.insert(skill.metadata.name.as_str()) {
                    bail!("duplicate bundled skill {}", skill.metadata.name);
                }
            }
            Ok(skills)
        })
}

fn skill_path(base_dir: &Path, name: &str) -> PathBuf {
    base_dir.join(name).join("SKILL.md")
}

fn parse_frontmatter(markdown: &str) -> Result<Frontmatter> {
    let rest = markdown
        .strip_prefix("---\n")
        .ok_or_else(|| eyre!("missing Agent Skills frontmatter start"))?;
    let (header, _) = rest
        .split_once("\n---\n")
        .ok_or_else(|| eyre!("missing Agent Skills frontmatter end"))?;
    let mut name = None;
    let mut description = None;
    for line in header.lines() {
        if let Some(value) = line.strip_prefix("name: ") {
            name = Some(value.trim().to_owned());
        }
        if let Some(value) = line.strip_prefix("description: ") {
            description = Some(value.trim().to_owned());
        }
    }
    let name = name.ok_or_else(|| eyre!("missing skill name"))?;
    let description = description.ok_or_else(|| eyre!("missing skill description"))?;
    if name.is_empty() || name.contains('/') || name.contains("..") {
        bail!("invalid skill name {name:?}");
    }
    if description.is_empty() {
        bail!("missing skill description");
    }
    Ok(Frontmatter { name, description })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_name_and_description_are_derived() {
        let listed = list().unwrap();
        assert_eq!(listed[0].name, "gander-review");
        assert!(listed[0].description.contains("durable local review state"));
        assert_eq!(listed[1].name, "gander-address-review");
    }

    #[test]
    fn duplicate_requested_names_are_prevented() {
        let err = select_skills(&["gander-review".into(), "gander-review".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate requested skill"));
    }

    #[test]
    fn exact_show_returns_embedded_bytes() {
        assert_eq!(
            show("gander-review").unwrap(),
            include_str!("../skills/gander-review/SKILL.md")
        );
    }

    #[test]
    fn preflight_is_atomic_and_writes_nothing_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let conflict = skill_path(dir.path(), "gander-review");
        fs::create_dir_all(conflict.parent().unwrap()).unwrap();
        fs::write(&conflict, "old").unwrap();
        let err = install(dir.path(), &[], false).unwrap_err().to_string();
        assert!(err.contains("refusing to overwrite"));
        assert!(!skill_path(dir.path(), "gander-address-review").exists());
        assert_eq!(fs::read_to_string(conflict).unwrap(), "old");
    }

    #[test]
    fn install_layout_and_content() {
        let dir = tempfile::tempdir().unwrap();
        let installed = install(dir.path(), &["gander-address-review".into()], false).unwrap();
        let path = dir.path().join("gander-address-review/SKILL.md");
        assert_eq!(
            installed,
            vec![InstalledSkill {
                name: "gander-address-review".into(),
                path: path.clone()
            }]
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            show("gander-address-review").unwrap()
        );
    }

    #[test]
    fn unknown_names_are_rejected_before_writes() {
        let dir = tempfile::tempdir().unwrap();
        let err = install(dir.path(), &["missing".into()], false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown skill missing"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn overwrite_requires_force() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &["gander-review".into()], false).unwrap();
        assert!(install(dir.path(), &["gander-review".into()], false).is_err());
        let installed = install(dir.path(), &["gander-review".into()], true).unwrap();
        assert_eq!(installed.len(), 1);
    }

    #[test]
    fn bundled_skills_are_bounded_and_have_cli_snippets() {
        for skill in bundled_skills().unwrap() {
            assert!(skill.markdown.len() <= MAX_SKILL_BYTES);
            assert!(skill.markdown.contains("```bash"));
            assert!(skill.markdown.contains("gander "));
        }
    }

    #[test]
    fn frontmatter_rejects_bad_names_and_missing_description() {
        assert!(parse_frontmatter("---\nname: ../x\ndescription: d\n---\n").is_err());
        assert!(parse_frontmatter("---\nname: x\n---\n").is_err());
    }
}
