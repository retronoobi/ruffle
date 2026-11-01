use ruffle_core::{backend::ui::*, font::FontQuery};
use url::Url;
pub struct CoreUi {
    visible: bool,
    clipboard: String,
}

impl CoreUi {
    pub fn new() -> Self {
        Self {
            visible: true,
            clipboard: String::new(),
        }
    }
}

impl UiBackend for CoreUi {
    fn mouse_visible(&self) -> bool {
        self.visible
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn set_mouse_cursor(&mut self, _cursor: MouseCursor) {}

    fn clipboard_content(&mut self) -> String {
        self.clipboard.clone()
    }

    fn set_clipboard_content(&mut self, content: String) {
        self.clipboard = content;
    }

    fn set_fullscreen(&mut self, _is_full: bool) -> Result<(), FullscreenError> {
        Ok(())
    }

    fn display_root_movie_download_failed_message(&self, _invalid_swf: bool, _fetch_error: String) {
    }

    fn message(&self, _message: &str) {}

    fn display_unsupported_video(&self, _url: Url) {}

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        crate::fonts::load(query, register);
    }

    fn sort_device_fonts(
        &self,
        _query: &FontQuery,
        _register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        Vec::new()
    }

    fn open_virtual_keyboard(&self) {}

    fn close_virtual_keyboard(&self) {}

    fn language(&self) -> LanguageIdentifier {
        US_ENGLISH.clone()
    }

    fn display_file_open_dialog(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<DialogResultFuture> {
        Some(Box::pin(async move { Ok(FileDialogResult::Canceled) }))
    }

    fn display_file_open_dialog_multiple(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<MultiDialogResultFuture> {
        Some(Box::pin(async move { Ok(MultiFileDialogResult::Canceled) }))
    }

    fn close_file_dialog(&mut self) {}

    fn display_file_save_dialog(
        &mut self,
        _file_name: String,
        _domain: String,
    ) -> Option<DialogResultFuture> {
        None
    }
}

impl Default for CoreUi {
    fn default() -> Self {
        CoreUi::new()
    }
}
