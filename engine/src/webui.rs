//! 内嵌的 Web UI（参考原桌面项目重绘；纯静态资源，无构建步骤）。
pub const INDEX_HTML: &str = include_str!("../web/index.html");
pub const APP_CSS: &str = include_str!("../web/app.css");
pub const APP_JS: &str = include_str!("../web/app.js");
