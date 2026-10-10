//! 界面文案集中管理；运行时根据持久化语言选择，不依赖系统全局语言。

pub fn text<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" { en } else { zh }
}
