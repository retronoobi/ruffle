//! Device fonts are read from Windows without depending on the desktop frontend.
use ruffle_core::{
    backend::ui::FontDefinition,
    font::{FontFileData, FontQuery},
    ttf_parser,
};
use std::{path::PathBuf, sync::OnceLock};
struct Face {
    path: PathBuf,
    index: u32,
    family: String,
    bold: bool,
    italic: bool,
}
fn catalog() -> &'static [Face] {
    static CATALOG: OnceLock<Vec<Face>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let root = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
            .join("Fonts");
        let mut fonts = vec![];
        let Ok(root) = crate::offline::local_path(&root) else {
            return fonts;
        };
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let Ok(path) = crate::offline::local_path(&entry.path()) else {
                    continue;
                };
                if !path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
                    ["ttf", "otf", "ttc"]
                        .iter()
                        .any(|e| s.eq_ignore_ascii_case(e))
                }) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                for index in 0..ttf_parser::fonts_in_collection(&bytes).unwrap_or(1) {
                    let Ok(face) = ttf_parser::Face::parse(&bytes, index) else {
                        continue;
                    };
                    let mut families = vec![];
                    for name in face.names() {
                        if [
                            ttf_parser::name_id::TYPOGRAPHIC_FAMILY,
                            ttf_parser::name_id::FAMILY,
                        ]
                        .contains(&name.name_id)
                            && let Some(name) = name.to_string()
                        {
                            let name = name.to_lowercase();
                            if !families.contains(&name) {
                                families.push(name);
                            }
                        }
                    }
                    for family in families {
                        fonts.push(Face {
                            path: path.clone(),
                            index,
                            family,
                            bold: face.is_bold(),
                            italic: face.is_italic(),
                        });
                    }
                }
            }
        }
        fonts
    })
}
pub fn load(query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
    let name = match query.lowercase_name.as_str() {
        "_sans" => "arial",
        "_serif" => "times new roman",
        "_typewriter" => "courier new",
        other => other,
    };
    let font = catalog()
        .iter()
        .filter(|f| f.family == name)
        .min_by_key(|f| {
            u8::from(f.bold != query.is_bold) * 2 + u8::from(f.italic != query.is_italic)
        });
    if let Some(font) = font {
        let Ok(path) = crate::offline::local_path(&font.path) else {
            return;
        };
        if let Ok(bytes) = std::fs::read(path) {
            register(FontDefinition::FontFile {
                name: query.name.clone(),
                is_bold: font.bold,
                is_italic: font.italic,
                data: FontFileData::new(bytes),
                index: font.index,
            });
        }
    }
}
