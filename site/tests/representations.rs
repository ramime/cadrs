use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    response::Response,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[path = "../src/app.rs"]
mod app;
#[path = "../src/components/mod.rs"]
// Vendored Proa UI source, kept byte for byte as `proa ui add` installed it (see fmt.sh);
// the site uses part of it.
#[allow(unused)]
mod components;
#[path = "../src/endpoints/mod.rs"]
mod endpoints;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/github.rs"]
mod github;
#[path = "../src/icons.rs"]
mod icons;
#[path = "../src/layouts/mod.rs"]
mod layouts;
#[path = "../src/pages/mod.rs"]
mod pages;
#[path = "../src/routes.rs"]
mod routes;
#[path = "../src/static_assets.rs"]
mod static_assets;
#[path = "../src/theme.rs"]
mod theme;

mod __proa_fonts_generated {
    include!(concat!(env!("OUT_DIR"), "/proa_fonts.rs"));
}

async fn body(response: Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

#[tokio::test(flavor = "multi_thread")]
async fn canonical_and_explicit_markdown_share_one_page_policy() {
    let app = routes::router(
        app::AppState,
        proa_core::BasePath::new("/apps/demo").unwrap(),
    );

    let html = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(html.status(), StatusCode::OK);
    assert!(html.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert_eq!(html.headers()[header::VARY], "Accept");
    assert!(html.headers()[header::LINK]
        .to_str()
        .unwrap()
        .contains("/apps/demo/index.md"));
    let document = String::from_utf8(body(html).await).unwrap();
    assert!(document.contains("href=\"/apps/demo/static/styles.css\""));
    assert!(document.contains("href=\"/apps/demo/\""));
    assert!(document.contains("href=\"/apps/demo/index.md\""));

    let refused_markdown = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::ACCEPT, "text/markdown;q=0, text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(refused_markdown.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/html"));

    let negotiated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::ACCEPT, "text/markdown")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(negotiated.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/markdown"));
    let negotiated = body(negotiated).await;

    let explicit = app
        .oneshot(
            Request::builder()
                .uri("/index.md")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(explicit.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/markdown"));
    let explicit = body(explicit).await;
    assert_eq!(negotiated, explicit);
    let markdown = String::from_utf8(explicit).unwrap();
    assert!(!markdown.contains("<html"));
    assert!(!markdown.contains("/static/"));
}

#[tokio::test(flavor = "multi_thread")]
async fn repository_assets_and_the_not_found_page_are_served() {
    let app = routes::router(app::AppState, proa_core::BasePath::new("").unwrap());
    let get = |uri: &'static str| {
        app.clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
    };

    // The page shows the README's screenshots and the app icon from the repository itself.
    for uri in [
        "/images/part-studio-light.webp",
        "/images/part-studio-dark.webp",
        "/images/documents-light.webp",
        "/images/schematic-dark.webp",
        "/images/render.png",
        "/icons/cadrs.png",
    ] {
        assert_eq!(get(uri).await.unwrap().status(), StatusCode::OK, "{uri}");
    }

    // `proa export` writes this page to 404.html.
    let missing = get("/no/such/page").await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert!(missing.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let document = String::from_utf8(body(missing).await).unwrap();
    assert!(document.contains("There is no page here"));
}
