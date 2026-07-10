use color_eyre::eyre::{Result, eyre};

pub const MIN_SELECTOR_LEN: usize = 8;

pub fn shortest_unique_prefix(id: &str, ids: &[&str]) -> String {
    let mut chars = 0;
    for (index, character) in id.char_indices() {
        chars += 1;
        if chars < MIN_SELECTOR_LEN {
            continue;
        }
        let end = index + character.len_utf8();
        let prefix = &id[..end];
        if ids.iter().filter(|other| other.starts_with(prefix)).count() <= 1 {
            return prefix.to_owned();
        }
    }
    id.to_owned()
}

pub fn resolve_unique_prefix<'a, T>(
    items: &'a [T],
    selector: &str,
    label: &str,
    id_of: impl Fn(&'a T) -> &'a str,
) -> Result<&'a T> {
    let matches = items
        .iter()
        .filter(|item| id_of(item).starts_with(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item] => Ok(item),
        [] => Err(eyre!("unknown {label} `{selector}`")),
        _ => Err(eyre!("ambiguous {label} prefix `{selector}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortest_unique_prefix_uses_eight_unless_collision_requires_more() {
        let ids = ["abcdef12-0000", "abcdef12-ffff", "12345678-0000"];
        assert_eq!(shortest_unique_prefix(ids[2], &ids), "12345678");
        assert_eq!(shortest_unique_prefix(ids[0], &ids), "abcdef12-0");
    }

    #[test]
    fn shortest_unique_prefix_respects_utf8_boundaries() {
        let ids = ["åßç∂éƒ©˙-one", "åßç∂éƒ©˙-two"];
        assert_eq!(shortest_unique_prefix(ids[0], &ids), "åßç∂éƒ©˙-o");
    }
}
