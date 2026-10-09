pub mod cpu;
pub mod details;
pub mod disk;
pub mod gpu;
pub mod memory;
pub mod network;

pub use cpu::{CpuCollector, CpuMetrics};
pub use details::{DetailMetrics, DetailsCollector};
pub use disk::{DiskCollector, DiskMetrics};
pub use gpu::{GpuCollector, GpuMetrics};
pub use memory::{MemoryCollector, MemoryMetrics};
pub use network::{NetworkCollector, NetworkSpeed};
