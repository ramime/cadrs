//! A small avatar with the user's initials, modeled on gpui-component's `Avatar` fallback.

use bevy::prelude::*;
use bevy::text::FontWeight;

use crate::theme::Theme;

/// Builder for an avatar.
pub struct Avatar {
    name: String,
    user: String,
    size: f32,
    color: Option<Color>,
}

impl Avatar {
    /// `name` is the node's `Name`; `user` is the person's display name.
    pub fn new(name: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            user: user.into(),
            size: 24.0,
            color: None,
        }
    }

    pub fn size(mut self, s: f32) -> Self {
        self.size = s;
        self
    }

    /// Its colour, rather than one picked from the user's name (a collaborator's colour).
    pub fn color(mut self, c: Color) -> Self {
        self.color = Some(c);
        self
    }

    /// Up to two initials from the display name.
    pub fn initials(user: &str) -> String {
        user.split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .flat_map(char::to_uppercase)
            .collect()
    }

    /// The stable muted colour picked from a user's name.
    pub fn color_for(user: &str) -> Color {
        const COLORS: [&str; 6] = [
            "#4f6d8f", "#6b5b95", "#3f7f6e", "#8a5a44", "#5b7a3a", "#7a4f6b",
        ];
        let hash = user
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        Color::Srgba(Srgba::hex(COLORS[hash as usize % COLORS.len()]).unwrap())
    }

    /// The initials' ink on `bg`: white, or near black on a light colour.
    pub fn ink_on(bg: Color) -> Color {
        let c = bg.to_linear();
        let luminance = 0.2126 * c.red + 0.7152 * c.green + 0.0722 * c.blue;
        if luminance > 0.36 {
            Color::srgb_u8(0x1a, 0x1a, 0x1a)
        } else {
            Color::WHITE
        }
    }

    pub fn build(self, theme: &Theme) -> impl Bundle {
        let bg = self.color.unwrap_or_else(|| Self::color_for(&self.user));
        (
            Name::new(self.name),
            Node {
                width: Val::Px(self.size),
                height: Val::Px(self.size),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(theme.radius)),
                flex_shrink: 0.0,
                ..default()
            },
            BackgroundColor(bg),
            children![(
                theme.text(
                    Self::initials(&self.user),
                    (self.size * 0.42).round(),
                    FontWeight::SEMIBOLD,
                    Self::ink_on(bg)
                ),
                Pickable::IGNORE,
            )],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials() {
        assert_eq!(Avatar::initials("Rouan van der Ende"), "RV");
        assert_eq!(Avatar::initials("casper"), "C");
        assert_eq!(Avatar::initials(""), "");
    }
}
