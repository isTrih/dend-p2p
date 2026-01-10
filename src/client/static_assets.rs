use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "src/client/static/"]
struct StaticAssets;

pub fn get_static_file(path: &str) -> Option<Cow<'static, [u8]>> {
    StaticAssets::get(path)
}

pub fn get_content_type(path: &str) -> &'static str {
    if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".js") {
        "application/javascript"
    } else if path.ends_with(".html") {
        "text/html"
    } else {
        "application/octet-stream"
    }
}
