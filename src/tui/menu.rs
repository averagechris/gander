//! Interactive menu-bar model: menu definitions, bar geometry, dropdown
//! geometry, and pure hit-testing.
//!
//! The bar itself is rendered by [`super::render`]; this module is the single
//! source of truth for which menus exist, which dropdown items they contain,
//! and where everything lands on screen, so rendering and mouse hit-testing
//! can never disagree. Menus and items whose action is unbound in the live
//! keymap are omitted entirely (matching the display-only bar behavior).

use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use super::keymap::{Action, KeyMap};

/// One top-level menu: the bar shows `"<hint> <title>"` for `bar_action`, and
/// clicking the title opens a dropdown of `items`.
pub(super) struct MenuDef {
    pub(super) title: &'static str,
    /// The action whose binding is displayed next to the title in the bar.
    /// An unbound bar action hides the whole menu (current bar behavior).
    pub(super) bar_action: Action,
    pub(super) items: &'static [MenuItemDef],
}

pub(super) struct MenuItemDef {
    pub(super) label: &'static str,
    pub(super) action: Action,
}

const fn item(label: &'static str, action: Action) -> MenuItemDef {
    MenuItemDef { label, action }
}

/// The menu bar contents. Bar titles/actions must stay in sync with the
/// pre-interactive bar so existing keymap discoverability is unchanged.
pub(super) const MENUS: &[MenuDef] = &[
    MenuDef {
        title: "help",
        bar_action: Action::Help,
        items: &[item("open help", Action::Help)],
    },
    MenuDef {
        title: "hunk",
        bar_action: Action::NextChangedHunk,
        items: &[
            item("next hunk", Action::NextChangedHunk),
            item("previous hunk", Action::PreviousChangedHunk),
        ],
    },
    MenuDef {
        title: "file",
        bar_action: Action::NextFile,
        items: &[
            item("next file", Action::NextFile),
            item("previous file", Action::PreviousFile),
            item("find file", Action::FileSearch),
        ],
    },
    MenuDef {
        title: "comment",
        bar_action: Action::Comment,
        items: &[
            item("new comment", Action::Comment),
            item("comment list", Action::CommentList),
            item("next comment", Action::NextComment),
            item("previous comment", Action::PreviousComment),
        ],
    },
    MenuDef {
        title: "view",
        bar_action: Action::ViewOptions,
        items: &[
            item("view options", Action::ViewOptions),
            item("side-by-side", Action::ToggleDiffView),
            item("wrap lines", Action::ToggleDiffWrap),
        ],
    },
    MenuDef {
        title: "focus",
        bar_action: Action::AttentionFocus,
        items: &[item("toggle focus preset", Action::AttentionFocus)],
    },
    MenuDef {
        title: "glance",
        bar_action: Action::AttentionGlance,
        items: &[item("attention glance", Action::AttentionGlance)],
    },
    MenuDef {
        title: "pane",
        bar_action: Action::ToggleFilePane,
        items: &[
            item("toggle file pane", Action::ToggleFilePane),
            item("widen file pane", Action::WidenFilePane),
            item("narrow file pane", Action::NarrowFilePane),
        ],
    },
    MenuDef {
        title: "quit",
        bar_action: Action::Quit,
        items: &[item("quit", Action::Quit)],
    },
];

/// Ephemeral dropdown state. This is view chrome, never a `Mode`: normal
/// review dispatch stays untouched while a dropdown is open, and any modal
/// opening closes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct MenuUiState {
    /// Index into [`MENUS`] of the open dropdown.
    pub(super) open: Option<usize>,
    /// Hovered dropdown item (index into the *bound* items).
    pub(super) hovered: Option<usize>,
}

impl MenuUiState {
    pub(super) fn open_menu(&mut self, index: usize) {
        self.open = Some(index);
        self.hovered = None;
    }

    /// Close any open dropdown; reports whether one was open (so Esc handling
    /// can treat the dropdown as the topmost transient layer).
    pub(super) fn close(&mut self) -> bool {
        let was_open = self.open.is_some();
        *self = Self::default();
        was_open
    }
}

/// One rendered bar entry: `text` is exactly what the bar shows for this
/// menu ("<hint> <title>") and `offset` is its column offset from the bar's
/// left edge (accounting for the leading space and two-space separators).
pub(super) struct MenuBarEntry {
    pub(super) menu_index: usize,
    pub(super) text: String,
    pub(super) offset: u16,
}

/// The bar entries for the live keymap, in render order. Menus whose bar
/// action is unbound are omitted.
pub(super) fn bar_entries(keymap: &KeyMap) -> Vec<MenuBarEntry> {
    let mut entries = Vec::new();
    let mut offset: u16 = 1; // leading space
    for (menu_index, menu) in MENUS.iter().enumerate() {
        let Some(hint) = keymap.bound_hint(menu.bar_action) else {
            continue;
        };
        if !entries.is_empty() {
            offset = offset.saturating_add(2); // separator
        }
        let text = format!("{hint} {}", menu.title);
        let width = text.width() as u16;
        entries.push(MenuBarEntry {
            menu_index,
            text,
            offset,
        });
        offset = offset.saturating_add(width);
    }
    entries
}

/// The clickable screen region of one bar entry, clamped to the bar area.
/// `None` when the bar is not rendered or the entry is fully clipped.
pub(super) fn title_region(entry: &MenuBarEntry, menu_area: Rect) -> Option<Rect> {
    if menu_area.height == 0 || menu_area.width < 80 {
        return None;
    }
    let x = menu_area.x.saturating_add(entry.offset);
    let right = menu_area.x.saturating_add(menu_area.width);
    if x >= right {
        return None;
    }
    let width = (entry.text.width() as u16).min(right - x);
    if width == 0 {
        return None;
    }
    Some(Rect::new(x, menu_area.y, width, 1))
}

/// The menu index under a pointer position, if any.
pub(super) fn title_at(keymap: &KeyMap, menu_area: Rect, x: u16, y: u16) -> Option<usize> {
    bar_entries(keymap).iter().find_map(|entry| {
        let rect = title_region(entry, menu_area)?;
        super::render::point_in_rect(x, y, rect).then_some(entry.menu_index)
    })
}

/// A dropdown row resolved against the live keymap: label plus the same
/// key hint the bar's source data uses. Unbound items are omitted.
pub(super) struct DropdownItem {
    pub(super) label: &'static str,
    pub(super) action: Action,
    pub(super) hint: String,
}

pub(super) fn dropdown_items(menu_index: usize, keymap: &KeyMap) -> Vec<DropdownItem> {
    let Some(menu) = MENUS.get(menu_index) else {
        return Vec::new();
    };
    menu.items
        .iter()
        .filter_map(|item| {
            keymap.bound_hint(item.action).map(|hint| DropdownItem {
                label: item.label,
                action: item.action,
                hint: hint.to_owned(),
            })
        })
        .collect()
}

/// The bordered dropdown rect for an open menu, anchored under its title and
/// clamped so it never overflows the terminal. `None` when the menu has no
/// bound items, the bar is hidden, or the terminal has no room for a single
/// item row.
pub(super) fn dropdown_rect(
    menu_index: usize,
    keymap: &KeyMap,
    menu_area: Rect,
    full_area: Rect,
) -> Option<Rect> {
    let items = dropdown_items(menu_index, keymap);
    if items.is_empty() {
        return None;
    }
    let entry_rect = bar_entries(keymap)
        .iter()
        .find(|entry| entry.menu_index == menu_index)
        .and_then(|entry| title_region(entry, menu_area))?;
    let inner_width = items
        .iter()
        // leading space + label + two-space gap + hint + trailing space
        .map(|item| item.label.width() + item.hint.width() + 4)
        .max()
        .unwrap_or(0)
        .max(MENUS.get(menu_index)?.title.width() + 2);
    let width = ((inner_width as u16).saturating_add(2)).min(full_area.width);
    if width < 4 {
        return None;
    }
    let y = menu_area.y.saturating_add(1);
    let bottom = full_area.y.saturating_add(full_area.height);
    let max_height = bottom.saturating_sub(y);
    let height = (items.len() as u16).saturating_add(2).min(max_height);
    if height < 3 {
        return None;
    }
    // Align the dropdown border one column left of the title text so item
    // labels line up under the title, then clamp inside the terminal.
    let right = full_area.x.saturating_add(full_area.width);
    let x = entry_rect
        .x
        .saturating_sub(1)
        .min(right.saturating_sub(width))
        .max(full_area.x);
    Some(Rect::new(x, y, width, height))
}

/// The dropdown item index under a pointer position, if any.
pub(super) fn dropdown_item_at(rect: Rect, item_count: usize, x: u16, y: u16) -> Option<usize> {
    let inner = super::render::inner_bordered(rect);
    if !super::render::point_in_rect(x, y, inner) {
        return None;
    }
    let index = (y - inner.y) as usize;
    (index < item_count).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeybindingsConfig;

    fn default_keymap() -> KeyMap {
        KeyMap::try_from(&KeybindingsConfig::default()).unwrap()
    }

    fn menu_index(title: &str) -> usize {
        MENUS.iter().position(|menu| menu.title == title).unwrap()
    }

    #[test]
    fn bar_entries_match_display_only_bar_text() {
        let keymap = default_keymap();
        let entries = bar_entries(&keymap);
        let mut text = String::from(" ");
        for (index, entry) in entries.iter().enumerate() {
            if index > 0 {
                text.push_str("  ");
            }
            assert_eq!(entry.offset as usize, text.width(), "{}", entry.text);
            text.push_str(&entry.text);
        }
        assert!(text.starts_with(" ? help  ] hunk  . file"), "{text}");
    }

    #[test]
    fn title_regions_hit_only_entry_text() {
        let keymap = default_keymap();
        let area = Rect::new(0, 0, 110, 1);
        let entries = bar_entries(&keymap);
        let first = title_region(&entries[0], area).unwrap();
        assert_eq!(first, Rect::new(1, 0, 6, 1)); // "? help"
        // Leading margin and inter-entry separator columns hit nothing.
        assert_eq!(title_at(&keymap, area, 0, 0), None);
        assert_eq!(title_at(&keymap, area, 1, 0), Some(0));
        assert_eq!(title_at(&keymap, area, 6, 0), Some(0));
        assert_eq!(title_at(&keymap, area, 7, 0), None);
        assert_eq!(title_at(&keymap, area, 8, 0), None);
        assert_eq!(title_at(&keymap, area, 9, 0), Some(menu_index("hunk")));
        // Off the bar row entirely.
        assert_eq!(title_at(&keymap, area, 1, 1), None);
    }

    #[test]
    fn hidden_bar_has_no_title_regions() {
        let keymap = default_keymap();
        assert_eq!(title_at(&keymap, Rect::new(0, 0, 110, 0), 1, 0), None);
        // Below the width gate the bar is never drawn, so nothing is clickable.
        assert_eq!(title_at(&keymap, Rect::new(0, 0, 79, 1), 1, 0), None);
    }

    #[test]
    fn unbound_bar_action_omits_menu_from_regions() {
        let config = KeybindingsConfig {
            next_changed_hunk: Vec::new(),
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let entries = bar_entries(&keymap);
        assert!(
            entries
                .iter()
                .all(|entry| entry.menu_index != menu_index("hunk"))
        );
        // The next menu slides left into the freed columns.
        let area = Rect::new(0, 0, 110, 1);
        assert_eq!(title_at(&keymap, area, 9, 0), Some(menu_index("file")));
    }

    #[test]
    fn dropdown_items_omit_unbound_actions() {
        let keymap = default_keymap();
        let items = dropdown_items(menu_index("view"), &keymap);
        let labels: Vec<&str> = items.iter().map(|item| item.label).collect();
        // "wrap lines" (ToggleDiffWrap) is unbound in the default preset.
        assert_eq!(labels, ["view options", "side-by-side"]);
        assert_eq!(items[0].hint, "V");
        assert_eq!(items[1].hint, "|");
    }

    #[test]
    fn dropdown_rect_is_anchored_under_title_and_clamped_to_terminal() {
        let keymap = default_keymap();
        let full = Rect::new(0, 0, 110, 24);
        let menu_area = Rect::new(0, 0, 110, 1);
        let hunk = menu_index("hunk");
        let rect = dropdown_rect(hunk, &keymap, menu_area, full).unwrap();
        let entries = bar_entries(&keymap);
        let title = entries
            .iter()
            .find(|entry| entry.menu_index == hunk)
            .and_then(|entry| title_region(entry, menu_area))
            .unwrap();
        assert_eq!(rect.x, title.x - 1);
        assert_eq!(rect.y, 1);
        assert_eq!(rect.height, 2 + 2); // two items + borders

        // Near the right edge the dropdown is clamped inside the terminal.
        let narrow = Rect::new(0, 0, 80, 24);
        let narrow_area = Rect::new(0, 0, 80, 1);
        let quit = menu_index("quit");
        let rect = dropdown_rect(quit, &keymap, narrow_area, narrow).unwrap();
        assert!(rect.x + rect.width <= 80, "{rect:?}");

        // Not enough vertical room for a single item: no dropdown, no panic.
        let tiny = Rect::new(0, 0, 110, 2);
        assert_eq!(dropdown_rect(hunk, &keymap, menu_area, tiny), None);
    }

    #[test]
    fn dropdown_rect_requires_bound_items_and_visible_bar() {
        let config = KeybindingsConfig {
            view_options: Vec::new(),
            toggle_diff_view: Vec::new(),
            toggle_diff_wrap: Vec::new(),
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let full = Rect::new(0, 0, 110, 24);
        let menu_area = Rect::new(0, 0, 110, 1);
        assert_eq!(
            dropdown_rect(menu_index("view"), &keymap, menu_area, full),
            None
        );
        let keymap = default_keymap();
        assert_eq!(
            dropdown_rect(menu_index("hunk"), &keymap, Rect::new(0, 0, 110, 0), full),
            None
        );
    }

    #[test]
    fn dropdown_item_hit_testing_respects_borders_and_item_count() {
        let rect = Rect::new(7, 1, 20, 4); // two items + borders
        assert_eq!(dropdown_item_at(rect, 2, 8, 2), Some(0));
        assert_eq!(dropdown_item_at(rect, 2, 25, 3), Some(1));
        // Borders and outside the rect are misses.
        assert_eq!(dropdown_item_at(rect, 2, 7, 2), None);
        assert_eq!(dropdown_item_at(rect, 2, 8, 1), None);
        assert_eq!(dropdown_item_at(rect, 2, 8, 4), None);
        assert_eq!(dropdown_item_at(rect, 2, 40, 2), None);
        // Rows past the item list (clipped dropdown) are misses.
        assert_eq!(dropdown_item_at(rect, 1, 8, 3), None);
    }

    #[test]
    fn menu_state_close_reports_whether_a_dropdown_was_open() {
        let mut state = MenuUiState::default();
        assert!(!state.close());
        state.open_menu(2);
        assert_eq!(state.open, Some(2));
        assert_eq!(state.hovered, None);
        state.hovered = Some(1);
        assert!(state.close());
        assert_eq!(state, MenuUiState::default());
    }
}
