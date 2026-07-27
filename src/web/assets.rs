use super::*;

pub(super) const COMPONENT_JS: &str = include_str!("../web.js");

pub(super) fn render_theme_css(config: &ThemeConfig) -> String {
    web_render::render_theme_css(config)
}

pub(super) fn read_extra_css(path: &std::path::Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read [web] extra-css stylesheet at {}",
            path.display()
        )
    })
}
