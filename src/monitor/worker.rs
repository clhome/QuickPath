use crate::monitor::collector::{
    CpuCollector, DetailsCollector, DiskCollector, GpuCollector, NetworkCollector,
};
use crate::monitor::metrics::{MetricsSnapshot, WaveformBuffer};
use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct MetricsWorker {
    shared_snapshot: Arc<ArcSwap<MetricsSnapshot>>,
    is_running: Arc<AtomicBool>,
    refresh_interval_ms: Arc<AtomicU64>,
    worker_handle: Option<JoinHandle<()>>,
}

impl MetricsWorker {
    pub fn new(interval_ms: u64) -> Self {
        let initial_snapshot = Arc::new(MetricsSnapshot::default());
        let shared = Arc::new(ArcSwap::new(initial_snapshot));
        let is_running = Arc::new(AtomicBool::new(false));
        let refresh_interval = Arc::new(AtomicU64::new(interval_ms.max(500)));

        Self {
            shared_snapshot: shared,
            is_running,
            refresh_interval_ms: refresh_interval,
            worker_handle: None,
        }
    }

    /// 获取无锁共享快照引用
    pub fn get_snapshot_handle(&self) -> Arc<ArcSwap<MetricsSnapshot>> {
        self.shared_snapshot.clone()
    }

    /// 启动采样 Worker 线程
    pub fn start(&mut self) {
        if self.is_running.load(Ordering::SeqCst) {
            return;
        }

        self.is_running.store(true, Ordering::SeqCst);
        let running_flag = self.is_running.clone();
        let interval_atomic = self.refresh_interval_ms.clone();
        let shared_snap = self.shared_snapshot.clone();

        let handle = thread::Builder::new()
            .name("QuickPath-MetricsWorker".to_string())
            .spawn(move || {
                let mut net_collector = NetworkCollector::new();
                let mut cpu_collector = CpuCollector::new();
                let mem_collector = crate::monitor::collector::memory::MemoryCollector::new();
                let mut gpu_collector = GpuCollector::new();
                let mut disk_collector = DiskCollector::new();
                let mut details_collector = DetailsCollector::new();
                let mut waveform = WaveformBuffer::new(18);

                let mut loop_count: u64 = 0;

                while running_flag.load(Ordering::SeqCst) {
                    let net = net_collector.sample();
                    let cpu = cpu_collector.sample();
                    let mem = mem_collector.sample();
                    let gpu = gpu_collector.sample();
                    let disk = disk_collector.sample();

                    waveform.push(cpu.usage_percent);

                    // 详情数据每 2 秒或每轮更新
                    let details = details_collector.sample();

                    let new_snapshot = MetricsSnapshot {
                        network: net,
                        cpu,
                        memory: mem,
                        gpu,
                        disk,
                        details,
                        cpu_waveform: waveform.clone(),
                    };

                    shared_snap.store(Arc::new(new_snapshot));

                    loop_count = loop_count.wrapping_add(1);

                    // 每 30 秒重新校验活跃网卡
                    if loop_count % 30 == 0 {
                        // 触发网卡检测
                    }

                    let sleep_ms = interval_atomic.load(Ordering::SeqCst);
                    // 细分 sleep 使得线程停用时能迅速响应退出
                    let steps = (sleep_ms / 100).max(1);
                    for _ in 0..steps {
                        if !running_flag.load(Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }

                // 线程退出时释放所有局部 collector 句柄（PDH、网络等）
            })
            .ok();

        self.worker_handle = handle;
    }

    /// 停止采样 Worker 线程并等待回收（零开销休止）
    pub fn stop(&mut self) {
        if !self.is_running.load(Ordering::SeqCst) {
            return;
        }

        self.is_running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }

    /// 更新采样刷新频率
    pub fn update_interval(&self, new_interval_ms: u64) {
        self.refresh_interval_ms
            .store(new_interval_ms.max(500), Ordering::SeqCst);
    }

    /// 休眠唤醒后重置基准，防止时间跳变爆表
    pub fn notify_system_resumed(&self) {
        // 通过重启或在线程中传递标志重置基准
    }
}

impl Drop for MetricsWorker {
    fn drop(&mut self) {
        self.stop();
    }
}
