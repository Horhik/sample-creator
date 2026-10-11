//! Interface language: Russian or English. The apps choose it (the system's
//! language unless the user picked one) and the engine's own texts — colour
//! names, export formats, error messages — follow.

use std::sync::atomic::{AtomicBool, Ordering};

static RUSSIAN: AtomicBool = AtomicBool::new(false);

pub fn set_russian(on: bool) {
    RUSSIAN.store(on, Ordering::Relaxed);
}

pub fn is_russian() -> bool {
    RUSSIAN.load(Ordering::Relaxed)
}

/// The text in the current language.
pub fn tr(ru: &'static str, en: &'static str) -> &'static str {
    if is_russian() { ru } else { en }
}

/// Unit after a number of seconds: "с" or "s".
pub fn sec() -> &'static str {
    tr("с", "s")
}

/// Whether a locale name such as `ru_RU.UTF-8` or `ru` is Russian.
pub fn is_russian_locale(name: &str) -> bool {
    name.get(..2).is_some_and(|l| l.eq_ignore_ascii_case("ru"))
}

/// The language from `LC_ALL`, `LC_MESSAGES` or `LANG`, as the C library
/// would pick it.
pub fn system_is_russian() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .is_some_and(|v| is_russian_locale(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_names() {
        assert!(is_russian_locale("ru_RU.UTF-8"));
        assert!(is_russian_locale("ru"));
        assert!(!is_russian_locale("en_US.UTF-8"));
        assert!(!is_russian_locale("C"));
        assert!(!is_russian_locale(""));
    }
}
