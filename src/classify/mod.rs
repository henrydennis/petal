//! What kind of thing a file or folder is (apps, caches, photos…), judged from its path
//! alone by a small model trained offline in `tools/path-classifier`. It colours the chart
//! by kind, where a wrong guess costs only a wrong colour; it never decides what's safe
//! to delete.

mod features;
mod model;

use std::path::Path;
use std::sync::LazyLock;

use gpui::{Hsla, rgb};
use palette::IntoColor;

pub use model::{CATEGORIES, Category};

static MODEL: LazyLock<model::Model> = LazyLock::new(|| {
    model::Model::from_bytes(include_bytes!("model.bin")).expect("the bundled model matches the feature code")
});

pub fn classify(path: &Path, is_file: bool) -> Category {
    MODEL.predict(&features::extract(&path.to_string_lossy(), is_file)).0
}

/// Chart colour for a kind. The seven hues are a categorical palette checked for
/// colour-blind and normal-vision separation on the chart's dark surface; kinds that
/// often sit side by side (Documents, Media, Downloads and Developer in the home folder;
/// Caches and App data in ~/Library) got the pairs that stay furthest apart. System and
/// Mixed are neutral so the meaningful kinds stand out.
pub fn color(category: Category) -> Hsla {
    let hex = match category {
        Category::Apps => 0xd95926,
        Category::Downloads => 0x3987e5,
        Category::Developer => 0x008300,
        Category::Caches => 0x199e70,
        Category::Media => 0xd55181,
        Category::Documents => 0xc98500,
        Category::AppData => 0x9085e9,
        Category::System => 0x5b5e66,
        Category::Mixed => 0x454850,
    };
    rgb(hex).into_color()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against a model file that no longer matches the feature code: the header
    /// check catches layout changes, these catch a stale or mistrained model.
    #[test]
    fn bundled_model_classifies_the_obvious() {
        let cases = [
            ("/System/Volumes/Data/Users/sam/Library/Caches/com.example.app", false, Category::Caches),
            ("/Users/sam/code/thing/node_modules", false, Category::Developer),
            ("/Users/sam/Downloads/Installer-1.2.dmg", true, Category::Downloads),
            ("/Users/sam/Pictures/Photos Library.photoslibrary", false, Category::Media),
            ("/Users/sam/Documents/Taxes", false, Category::Documents),
            ("/System/Volumes/Data/Applications/Example.app", false, Category::Apps),
            ("/Users/sam/Library/Messages", false, Category::AppData),
            ("/System/Volumes/Data/private/var/db", false, Category::System),
            ("/System/Volumes/Data/Users/sam", false, Category::Mixed),
        ];
        for (path, is_file, expected) in cases {
            assert_eq!(classify(Path::new(path), is_file), expected, "{path}");
        }
    }
}
