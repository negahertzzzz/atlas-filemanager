#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandMeta {
    pub(crate) id: &'static str,
    pub(crate) default_shortcut: Option<&'static str>,
    pub(crate) remappable: bool,
}

impl CommandMeta {
    const fn remappable(id: &'static str, default_shortcut: Option<&'static str>) -> Self {
        Self {
            id,
            default_shortcut,
            remappable: true,
        }
    }
}

pub(crate) const COMMANDS: &[CommandMeta] = &[
    CommandMeta::remappable("files.convert", Some("ctrl-alt-c")),
    CommandMeta::remappable("tab.new", Some("ctrl-t")),
    CommandMeta::remappable("tab.close", Some("ctrl-w")),
    CommandMeta::remappable("tab.reopen", Some("ctrl-shift-t")),
    CommandMeta::remappable("tab.detach", None),
    CommandMeta::remappable("tab.next", Some("ctrl-tab")),
    CommandMeta::remappable("tab.previous", Some("ctrl-shift-tab")),
    CommandMeta::remappable("tab.move-left", Some("ctrl-shift-pageup")),
    CommandMeta::remappable("tab.move-right", Some("ctrl-shift-pagedown")),
    CommandMeta::remappable("tab.1", Some("ctrl-1")),
    CommandMeta::remappable("tab.2", Some("ctrl-2")),
    CommandMeta::remappable("tab.3", Some("ctrl-3")),
    CommandMeta::remappable("tab.4", Some("ctrl-4")),
    CommandMeta::remappable("tab.5", Some("ctrl-5")),
    CommandMeta::remappable("tab.6", Some("ctrl-6")),
    CommandMeta::remappable("tab.7", Some("ctrl-7")),
    CommandMeta::remappable("tab.8", Some("ctrl-8")),
    CommandMeta::remappable("tab.9", Some("ctrl-9")),
    CommandMeta::remappable("pane.split-h", Some("ctrl-shift-s")),
    CommandMeta::remappable("pane.split-v", Some("ctrl-alt-s")),
    CommandMeta::remappable("pane.close", Some("ctrl-shift-w")),
    CommandMeta::remappable("pane.focus-next", Some("f6")),
    CommandMeta::remappable("pane.focus-left", Some("ctrl-alt-left")),
    CommandMeta::remappable("pane.focus-right", Some("ctrl-alt-right")),
    CommandMeta::remappable("pane.focus-above", Some("ctrl-alt-up")),
    CommandMeta::remappable("pane.focus-below", Some("ctrl-alt-down")),
    CommandMeta::remappable("pane.even-siblings", None),
    CommandMeta::remappable("pane.even-all", None),
    CommandMeta::remappable("pane.move-left", Some("ctrl-alt-shift-left")),
    CommandMeta::remappable("pane.move-right", Some("ctrl-alt-shift-right")),
    CommandMeta::remappable("pane.move-above", Some("ctrl-alt-shift-up")),
    CommandMeta::remappable("pane.move-below", Some("ctrl-alt-shift-down")),
    CommandMeta::remappable("nav.back", Some("alt-left")),
    CommandMeta::remappable("nav.forward", Some("alt-right")),
    CommandMeta::remappable("nav.parent", Some("alt-up")),
    CommandMeta::remappable("nav.goto", Some("ctrl-k")),
    CommandMeta::remappable("nav.refresh", Some("f5")),
    #[cfg(target_os = "windows")]
    CommandMeta::remappable("nav.recycle-bin", None),
    #[cfg(target_os = "windows")]
    CommandMeta::remappable("net.map-drive", None),
    CommandMeta::remappable("pane.compare", None),
    CommandMeta::remappable("git.open", None),
    CommandMeta::remappable("view.hidden", Some("ctrl-.")),
    CommandMeta::remappable("view.vim-mode", None),
    CommandMeta::remappable("view.inspector", Some("ctrl-i")),
    CommandMeta::remappable("view.branch-view", Some("ctrl-b")),
    CommandMeta::remappable("view.sidebar", None),
    CommandMeta::remappable("view.sidebar-collapse", Some("ctrl-alt-b")),
    CommandMeta::remappable("view.details", None),
    CommandMeta::remappable("view.grid", None),
    CommandMeta::remappable("view.list", None),
    CommandMeta::remappable("view.miller", None),
    CommandMeta::remappable("view.tree", None),
    CommandMeta::remappable("view.group-none", None),
    CommandMeta::remappable("view.group-date", None),
    CommandMeta::remappable("view.group-size", None),
    CommandMeta::remappable("view.group-type", None),
    CommandMeta::remappable("view.group-name", None),
    CommandMeta::remappable("view.group-created", None),
    CommandMeta::remappable("view.group-accessed", None),
    CommandMeta::remappable("view.group-deleted", None),
    CommandMeta::remappable("view.group-origin", None),
    CommandMeta::remappable("view.group-expand", None),
    CommandMeta::remappable("view.group-collapse", None),
    CommandMeta::remappable("view.theme-toggle", Some("ctrl-shift-l")),
    CommandMeta::remappable("view.theme-follow-system", None),
    CommandMeta::remappable("view.zoom-in", Some("ctrl-=")),
    CommandMeta::remappable("view.zoom-out", Some("ctrl--")),
    CommandMeta::remappable("view.zoom-reset", Some("ctrl-0")),
    CommandMeta::remappable("view.content-zoom-in", None),
    CommandMeta::remappable("view.content-zoom-out", None),
    CommandMeta::remappable("folder.toggle-folders-first", None),
    CommandMeta::remappable("folder.inherit-view", None),
    CommandMeta::remappable("folder.toggle-files", None),
    CommandMeta::remappable("folder.toggle-folders", None),
    CommandMeta::remappable("folder.search", Some("ctrl-f")),
    CommandMeta::remappable("folder.search-subfolders", Some("ctrl-shift-f")),
    CommandMeta::remappable("selection.previous", Some("up")),
    CommandMeta::remappable("selection.next", Some("down")),
    CommandMeta::remappable("selection.left", Some("left")),
    CommandMeta::remappable("selection.right", Some("right")),
    CommandMeta::remappable("selection.first", Some("home")),
    CommandMeta::remappable("selection.last", Some("end")),
    CommandMeta::remappable("selection.page-up", Some("pageup")),
    CommandMeta::remappable("selection.page-down", Some("pagedown")),
    CommandMeta::remappable("selection.extend-previous", Some("shift-up")),
    CommandMeta::remappable("selection.extend-next", Some("shift-down")),
    CommandMeta::remappable("selection.extend-left", Some("shift-left")),
    CommandMeta::remappable("selection.extend-right", Some("shift-right")),
    CommandMeta::remappable("selection.extend-first", Some("shift-home")),
    CommandMeta::remappable("selection.extend-last", Some("shift-end")),
    CommandMeta::remappable("selection.extend-page-up", Some("shift-pageup")),
    CommandMeta::remappable("selection.extend-page-down", Some("shift-pagedown")),
    CommandMeta::remappable("selection.cursor-previous", Some("ctrl-up")),
    CommandMeta::remappable("selection.cursor-next", Some("ctrl-down")),
    CommandMeta::remappable("selection.cursor-left", Some("ctrl-left")),
    CommandMeta::remappable("selection.cursor-right", Some("ctrl-right")),
    CommandMeta::remappable("selection.toggle", Some("ctrl-space")),
    CommandMeta::remappable("selection.open", Some("enter")),
    CommandMeta::remappable("edit.select-all", Some("ctrl-a")),
    CommandMeta::remappable("edit.invert", Some("ctrl-shift-i")),
    CommandMeta::remappable("edit.undo", Some("ctrl-z")),
    CommandMeta::remappable("edit.redo", Some("ctrl-shift-z")),
    CommandMeta::remappable("edit.batch-rename", None),
    CommandMeta::remappable("files.rename", Some("f2")),
    CommandMeta::remappable("files.copy", Some("ctrl-c")),
    CommandMeta::remappable("files.cut", Some("ctrl-x")),
    CommandMeta::remappable("files.paste", Some("ctrl-v")),
    CommandMeta::remappable("files.open-terminal", None),
    CommandMeta::remappable("files.create-folder", Some("ctrl-shift-n")),
    CommandMeta::remappable("files.create-file", Some("ctrl-alt-n")),
    CommandMeta::remappable("files.copy-path", Some("ctrl-shift-c")),
    #[cfg(windows)]
    CommandMeta::remappable("files.paste-shortcut", None),
    CommandMeta::remappable("files.properties", Some("alt-enter")),
    CommandMeta::remappable("files.context-menu", Some("shift-f10")),
    CommandMeta::remappable("files.trash", Some("delete")),
    CommandMeta::remappable("files.delete", Some("shift-delete")),
    CommandMeta::remappable("files.compress", None),
    CommandMeta::remappable("files.open-dirs-in-tabs", Some("ctrl-enter")),
    CommandMeta::remappable("files.open-in-window", None),
    CommandMeta::remappable("files.open-dir-right-split", Some("ctrl-shift-enter")),
    CommandMeta::remappable("files.open-dir-bottom-split", None),
    CommandMeta::remappable("recycle.restore", None),
    CommandMeta::remappable("recycle.purge", None),
    CommandMeta::remappable("recycle.empty", None),
    CommandMeta::remappable("profile.new", None),
    CommandMeta::remappable("profile.rename", None),
    CommandMeta::remappable("profile.delete", None),
    CommandMeta::remappable("ext.manager", Some("ctrl-shift-x")),
    CommandMeta::remappable("sys.palette", Some("ctrl-shift-p")),
    CommandMeta::remappable("sys.settings", Some("ctrl-,")),
    CommandMeta::remappable("sys.guide", Some("f1")),
];

pub(crate) fn command_meta(command_id: &str) -> Option<&'static CommandMeta> {
    COMMANDS.iter().find(|command| command.id == command_id)
}

pub(crate) fn command_remappable(command_id: &str) -> bool {
    command_meta(command_id).is_some_and(|command| command.remappable)
}

pub(crate) fn default_command_shortcut(command_id: &str) -> Option<&'static str> {
    command_meta(command_id).and_then(|command| command.default_shortcut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ids_are_unique() {
        let mut ids: Vec<&str> = COMMANDS.iter().map(|command| command.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len());
    }

    #[test]
    fn command_without_default_can_still_be_remapped() {
        assert_eq!(default_command_shortcut("view.details"), None);
        assert!(command_remappable("view.details"));
    }

    #[test]
    fn theme_commands_are_registered_and_remappable() {
        assert_eq!(default_command_shortcut("view.theme-toggle"), Some("ctrl-shift-l"));
        assert!(command_remappable("view.theme-toggle"));
        assert_eq!(default_command_shortcut("view.theme-follow-system"), None);
        assert!(command_remappable("view.theme-follow-system"));
        let taken = COMMANDS
            .iter()
            .filter(|command| command.default_shortcut == Some("ctrl-shift-l"))
            .count();
        assert_eq!(taken, 1, "ctrl-shift-l must not collide with another default");
    }

    #[test]
    fn vim_mode_command_is_registered_and_remappable() {
        assert_eq!(default_command_shortcut("view.vim-mode"), None);
        assert!(command_remappable("view.vim-mode"));
    }
}
