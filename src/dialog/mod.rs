pub mod detector;
pub mod injector;

pub use detector::{detect_file_dialog, DialogKind};
pub use injector::inject_path_to_dialog;
