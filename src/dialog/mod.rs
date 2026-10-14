pub mod detector;
pub mod injector;

pub use detector::{DialogKind, detect_file_dialog};
pub use injector::{InjectDone, inject_path_to_dialog};
