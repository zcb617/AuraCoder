use std::env;

use sys_locale::get_locale as get_system_locale;

pub const DEFAULT_APP_LOCALE: &str = "en";
pub const PT_BR_APP_LOCALE: &str = "pt-BR";
pub const ZH_CN_APP_LOCALE: &str = "zh-CN";

pub fn tray_menu_strings(locale: &str) -> (&'static str, &'static str) {
    match normalize_app_locale(locale).unwrap_or(DEFAULT_APP_LOCALE) {
        PT_BR_APP_LOCALE => ("Abrir AuraCoder", "Sair do AuraCoder"),
        ZH_CN_APP_LOCALE => ("打开 AuraCoder", "退出 AuraCoder"),
        _ => ("Open AuraCoder", "Quit AuraCoder"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledExitConfirmationStrings {
    pub title: &'static str,
    pub message: &'static str,
    pub confirm: &'static str,
    pub cancel: &'static str,
}

pub fn scheduled_exit_confirmation_strings(locale: &str) -> ScheduledExitConfirmationStrings {
    match normalize_app_locale(locale).unwrap_or(DEFAULT_APP_LOCALE) {
        PT_BR_APP_LOCALE => ScheduledExitConfirmationStrings {
            title: "Sair do AuraCoder?",
            message: "Há tarefas agendadas ativas. Elas não serão executadas enquanto o AuraCoder estiver fechado. Tem certeza de que deseja sair?",
            confirm: "Sair",
            cancel: "Cancelar",
        },
        ZH_CN_APP_LOCALE => ScheduledExitConfirmationStrings {
            title: "确认退出 AuraCoder？",
            message: "当前有启用中的计划任务。退出 AuraCoder 后，这些任务将无法按计划执行。确定要退出吗？",
            confirm: "退出",
            cancel: "取消",
        },
        _ => ScheduledExitConfirmationStrings {
            title: "Quit AuraCoder?",
            message: "There are enabled scheduled tasks. They cannot run after AuraCoder exits. Are you sure you want to quit?",
            confirm: "Quit",
            cancel: "Cancel",
        },
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy)]
pub struct NativeStrings {
    pub app_menu: &'static str,
    pub about_comments: &'static str,
    pub edit_menu: &'static str,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub undo: &'static str,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub redo: &'static str,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub cut: &'static str,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub copy: &'static str,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub paste: &'static str,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub select_all: &'static str,
    pub view_menu: &'static str,
    pub window_menu: &'static str,
    pub toggle_sidebar: &'static str,
    pub toggle_git_panel: &'static str,
    pub toggle_focus_mode: &'static str,
    pub toggle_fullscreen: &'static str,
    pub search: &'static str,
    pub toggle_terminal: &'static str,
    pub close: &'static str,
}

pub fn normalize_app_locale(input: &str) -> Option<&'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_encoding = trimmed.split('.').next().unwrap_or(trimmed);
    let without_variant = without_encoding
        .split('@')
        .next()
        .unwrap_or(without_encoding);
    let normalized = without_variant.replace('_', "-").to_ascii_lowercase();

    if normalized == "pt" || normalized.starts_with("pt-") {
        Some(PT_BR_APP_LOCALE)
    } else if normalized == "zh"
        || normalized == "zh-cn"
        || normalized == "zh-sg"
        || normalized.starts_with("zh-hans")
    {
        Some(ZH_CN_APP_LOCALE)
    } else if normalized == "en" || normalized.starts_with("en-") {
        Some(DEFAULT_APP_LOCALE)
    } else {
        None
    }
}

pub fn detect_system_locale() -> Option<&'static str> {
    if let Some(locale) = get_system_locale().and_then(|value| normalize_app_locale(&value)) {
        return Some(locale);
    }

    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| {
            env::var(key)
                .ok()
                .and_then(|value| normalize_app_locale(&value))
        })
}

pub fn resolve_app_locale(saved_locale: Option<&str>) -> &'static str {
    resolve_app_locale_with_system(saved_locale, detect_system_locale())
}

fn resolve_app_locale_with_system(
    saved_locale: Option<&str>,
    system_locale: Option<&str>,
) -> &'static str {
    saved_locale
        .and_then(normalize_app_locale)
        .or_else(|| system_locale.and_then(normalize_app_locale))
        .unwrap_or(DEFAULT_APP_LOCALE)
}

#[cfg(any(target_os = "macos", test))]
pub fn native_strings(locale: &str) -> NativeStrings {
    match normalize_app_locale(locale).unwrap_or(DEFAULT_APP_LOCALE) {
        PT_BR_APP_LOCALE => NativeStrings {
            app_menu: "AuraCoder",
            about_comments: "O cockpit open-source para programacao com assistencia de IA",
            edit_menu: "Editar",
            undo: "Desfazer",
            redo: "Refazer",
            cut: "Recortar",
            copy: "Copiar",
            paste: "Colar",
            select_all: "Selecionar tudo",
            view_menu: "Visualizar",
            window_menu: "Janela",
            toggle_sidebar: "Alternar barra lateral",
            toggle_git_panel: "Alternar painel Git",
            toggle_focus_mode: "Alternar modo foco",
            toggle_fullscreen: "Alternar tela cheia",
            search: "Buscar no workspace",
            toggle_terminal: "Alternar terminal",
            close: "Fechar",
        },
        ZH_CN_APP_LOCALE => NativeStrings {
            app_menu: "AuraCoder",
            about_comments: "面向 AI 辅助编程的开源工作台",
            edit_menu: "编辑",
            undo: "撤销",
            redo: "重做",
            cut: "剪切",
            copy: "复制",
            paste: "粘贴",
            select_all: "全选",
            view_menu: "视图",
            window_menu: "窗口",
            toggle_sidebar: "切换侧边栏",
            toggle_git_panel: "切换 Git 面板",
            toggle_focus_mode: "切换专注模式",
            toggle_fullscreen: "切换全屏",
            search: "搜索工作区",
            toggle_terminal: "切换终端",
            close: "关闭",
        },
        _ => NativeStrings {
            app_menu: "AuraCoder",
            about_comments: "The open-source cockpit for AI-assisted coding",
            edit_menu: "Edit",
            undo: "Undo",
            redo: "Redo",
            cut: "Cut",
            copy: "Copy",
            paste: "Paste",
            select_all: "Select All",
            view_menu: "View",
            window_menu: "Window",
            toggle_sidebar: "Toggle Sidebar",
            toggle_git_panel: "Toggle Git Panel",
            toggle_focus_mode: "Toggle Focus Mode",
            toggle_fullscreen: "Toggle Full Screen",
            search: "Search Workspace",
            toggle_terminal: "Toggle Terminal",
            close: "Close",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        native_strings, normalize_app_locale, resolve_app_locale_with_system,
        scheduled_exit_confirmation_strings, tray_menu_strings, DEFAULT_APP_LOCALE,
        PT_BR_APP_LOCALE, ZH_CN_APP_LOCALE,
    };

    #[test]
    fn normalizes_supported_locales() {
        assert_eq!(normalize_app_locale("en"), Some(DEFAULT_APP_LOCALE));
        assert_eq!(normalize_app_locale("en-US"), Some(DEFAULT_APP_LOCALE));
        assert_eq!(normalize_app_locale("pt"), Some(PT_BR_APP_LOCALE));
        assert_eq!(normalize_app_locale("pt_BR.UTF-8"), Some(PT_BR_APP_LOCALE));
        assert_eq!(normalize_app_locale("zh"), Some(ZH_CN_APP_LOCALE));
        assert_eq!(
            normalize_app_locale("zh_Hans_CN.UTF-8"),
            Some(ZH_CN_APP_LOCALE)
        );
        assert_eq!(normalize_app_locale("zh-TW"), None);
    }

    #[test]
    fn resolves_saved_locale_before_system_locale() {
        assert_eq!(
            resolve_app_locale_with_system(Some("en-US"), Some("pt-BR")),
            DEFAULT_APP_LOCALE
        );
        assert_eq!(
            resolve_app_locale_with_system(Some("pt"), Some("en-US")),
            PT_BR_APP_LOCALE
        );
        assert_eq!(
            resolve_app_locale_with_system(Some("zh-CN"), Some("en-US")),
            ZH_CN_APP_LOCALE
        );
    }

    #[test]
    fn resolves_system_locale_before_default() {
        assert_eq!(
            resolve_app_locale_with_system(Some("fr-FR"), Some("pt-BR")),
            PT_BR_APP_LOCALE
        );
        assert_eq!(
            resolve_app_locale_with_system(None, Some("en_US.UTF-8")),
            DEFAULT_APP_LOCALE
        );
        assert_eq!(
            resolve_app_locale_with_system(Some("fr-FR"), Some("de-DE")),
            DEFAULT_APP_LOCALE
        );
    }

    #[test]
    fn returns_pt_br_native_strings() {
        let strings = native_strings("pt-BR");

        assert_eq!(strings.edit_menu, "Editar");
        assert_eq!(strings.close, "Fechar");
    }
    #[test]
    fn returns_zh_cn_native_strings() {
        let strings = native_strings("zh-CN");

        assert_eq!(strings.edit_menu, "编辑");
        assert_eq!(strings.close, "关闭");
    }

    #[test]
    fn returns_localized_tray_menu_strings() {
        assert_eq!(
            tray_menu_strings("en"),
            ("Open AuraCoder", "Quit AuraCoder")
        );
        assert_eq!(
            tray_menu_strings("pt-BR"),
            ("Abrir AuraCoder", "Sair do AuraCoder")
        );
        assert_eq!(
            tray_menu_strings("zh-CN"),
            ("打开 AuraCoder", "退出 AuraCoder")
        );
    }

    #[test]
    fn returns_localized_scheduled_exit_confirmation_strings() {
        let en = scheduled_exit_confirmation_strings("en");
        assert_eq!(en.title, "Quit AuraCoder?");
        assert_eq!(en.confirm, "Quit");
        assert_eq!(en.cancel, "Cancel");

        let pt = scheduled_exit_confirmation_strings("pt-BR");
        assert_eq!(pt.title, "Sair do AuraCoder?");
        assert_eq!(pt.confirm, "Sair");
        assert_eq!(pt.cancel, "Cancelar");

        let zh = scheduled_exit_confirmation_strings("zh-CN");
        assert_eq!(zh.title, "确认退出 AuraCoder？");
        assert_eq!(zh.confirm, "退出");
        assert_eq!(zh.cancel, "取消");
    }
}
