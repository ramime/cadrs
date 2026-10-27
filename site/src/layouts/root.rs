// proa:document-root
use std::future::Future;

use proa_core::{
    raw_text_ok, ClassList, DataLoader, RenderOutcome, StaticRaw, WebContext, WebRender, WriteError,
};
use proa_fonts::Font;
use proa_fonts_macros::google_font;
use proa_macros::html;

/// The app's own typeface.
static SANS: Font = google_font!(
    "Inter",
    subsets = [latin],
    weights = [400, 500, 600, 700],
    variable = "--font-inter"
);

pub struct RootDocument<C> {
    pub title: &'static str,
    pub description: &'static str,
    pub content: C,
}

impl<L, C> WebRender<L> for RootDocument<C>
where
    L: DataLoader,
    C: WebRender<L> + Send + 'static,
{
    fn render(
        self,
        cx: &mut WebContext<L>,
    ) -> RenderOutcome<impl Future<Output = Result<(), WriteError>>> {
        RenderOutcome::pending(async move {
            let RootDocument {
                title,
                description,
                content,
            } = self;
            let icon = cx.request().url("/icons/cadrs.png");
            let stylesheet = cx.request().url("/static/styles.css");
            let html_class = ClassList::<1>::new().add(SANS.class_name());
            let fonts = proa_core::to_async(FontsHead);
            html! {
                <!DOCTYPE html>
                <html lang="en" class={html_class}>
                    <head>
                        <meta charset="utf-8" />
                        <meta name="viewport" content="width=device-width, initial-scale=1" />
                        <meta name="color-scheme" content="light dark" />
                        <meta name="description" content={description} />
                        <meta name="theme-color" content="#1d4fb8" />
                        <meta name="vibetrend-verify" content="vt-05ddd28bae9121b79d74bde2" />
                        {StaticRaw(crate::theme::THEME_SCRIPT)}
                        {StaticRaw(crate::github::STARS_SCRIPT)}
                        {StaticRaw(crate::pages::home::RELEASE_SCRIPT)}
                        <title>{raw_text_ok(title)}</title>
                        <link rel="icon" type="image/png" href={icon} />
                        {fonts}
                        <link rel="stylesheet" href={stylesheet} />
                    </head>
                    <body>{content}</body>
                </html>
            }
            .render(cx)
            .resolve()
            .await
        })
    }
}

struct FontsHead;

impl<L: DataLoader> proa_core::WebRenderSync<L> for FontsHead {
    fn render(self, cx: &mut WebContext<L>) -> Result<(), WriteError> {
        SANS.head(cx)
    }
}
